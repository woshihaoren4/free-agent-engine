use crate::protocol::{DEFAULT_PORT, ServerConfig};
use anyhow::Context;
use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub struct FileLock(File);

/// A user service must not create root-owned credentials in another user's home.
/// Keep administrative stop/status available so an existing sudo launch can be recovered.
pub fn check_home_owner(home: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owner = std::fs::metadata(home)
            .with_context(|| format!("inspect FAE home {}", home.display()))?
            .uid();
        let user = unsafe { libc::geteuid() };
        anyhow::ensure!(
            owner == user,
            "Cannot start service in {}: FAE home belongs to UID {owner}, but this process runs as UID {user}. \
             Run as the home owner without sudo; if the port requires privileges, use --port {DEFAULT_PORT}. \
             To recover an existing sudo launch, stop it using sudo with the same --fae-home first.",
            home.display()
        );
    }
    let _ = home;
    Ok(())
}

pub fn runtime_file_error(path: &Path, error: std::io::Error) -> anyhow::Error {
    let advice = if error.kind() == std::io::ErrorKind::PermissionDenied {
        " Check file ownership and permissions. If the service was started with sudo, \
         stop it using sudo with the same --fae-home, then restart as the home owner. \
         Keep server.token private."
    } else {
        ""
    };
    anyhow::Error::new(error).context(format!(
        "Cannot access service file {}.{advice}",
        path.display()
    ))
}

impl FileLock {
    pub fn try_acquire(path: &Path) -> anyhow::Result<Option<Self>> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| runtime_file_error(path, error))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self(file))),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub async fn acquire(path: PathBuf) -> anyhow::Result<Self> {
        tokio::task::spawn_blocking(move || {
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|error| runtime_file_error(&path, error))?;
            file.lock_exclusive()?;
            Ok(Self(file))
        })
        .await?
    }
}
impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub fn read_config(home: &Path) -> anyhow::Result<ServerConfig> {
    let path = home.join("server.config");
    let bytes = std::fs::read(&path).map_err(|error| runtime_file_error(&path, error))?;
    let config: ServerConfig = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        config.pid > 0
            && config.pid <= i32::MAX as u32
            && config.port > 0
            && !config.instance_id.is_empty(),
        "invalid server.config PID, port or instance"
    );
    Ok(config)
}

pub fn atomic_write(path: &Path, bytes: &[u8], private: bool) -> anyhow::Result<()> {
    let temp = path.with_extension(format!("tmp-{}", wd_tools::uuid::v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if private { 0o600 } else { 0o644 });
        }
        let mut file = options.open(&temp)?;
        #[cfg(windows)]
        if private {
            let user = std::env::var("USERNAME")?;
            let status = std::process::Command::new("icacls")
                .arg(&temp)
                .args(["/inheritance:r", "/grant:r", &format!("{user}:F")])
                .output()?;
            anyhow::ensure!(status.status.success(), "restrict token ACL failed");
        }
        let _ = private;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        #[cfg(not(windows))]
        std::fs::rename(&temp, path)?;
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
            let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let ok = unsafe {
                windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                    from.as_ptr(),
                    to.as_ptr(),
                    1 | 8,
                )
            };
            anyhow::ensure!(
                ok != 0,
                "publish runtime file: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

pub fn publish(home: &Path, config: &ServerConfig, token: &str) -> anyhow::Result<()> {
    atomic_write(&home.join("server.token"), token.as_bytes(), true)?;
    atomic_write(
        &home.join("server.config"),
        &serde_json::to_vec_pretty(config)?,
        false,
    )
}

pub fn remove_owned(home: &Path, instance: &str) -> anyhow::Result<()> {
    if read_config(home).is_ok_and(|config| config.instance_id == instance) {
        clear_discovery(home)?;
    }
    Ok(())
}
pub fn clear_discovery(home: &Path) -> anyhow::Result<()> {
    for name in ["server.config", "server.token"] {
        match std::fs::remove_file(home.join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

pub fn process_alive(pid: u32) -> anyhow::Result<bool> {
    #[cfg(unix)]
    {
        if unsafe { libc::kill(pid as i32, 0) } == 0 {
            // kill(pid, 0) also succeeds for an exited child awaiting its parent.
            // ps exposes that state without reaping a child owned by a caller.
            let status = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()?;
            if status.status.success()
                && String::from_utf8_lossy(&status.stdout)
                    .trim_start()
                    .starts_with('Z')
            {
                return Ok(false);
            }
            return Ok(true);
        }
        return match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            Some(libc::EPERM) => Ok(true),
            _ => Err(std::io::Error::last_os_error().into()),
        };
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{Foundation::*, System::Threading::*};
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            let error = std::io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(87) => Ok(false),
                Some(5) => Ok(true),
                _ => Err(error.into()),
            };
        }
        let mut code = 0;
        let ok = unsafe { GetExitCodeProcess(handle, &mut code) };
        unsafe {
            CloseHandle(handle);
        }
        anyhow::ensure!(ok != 0, "query process failed");
        Ok(code == 259)
    }
}

pub fn log_file(home: &Path) -> anyhow::Result<File> {
    let dir = home.join("log");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("server.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() >= 8 * 1024 * 1024) {
        let previous = dir.join("server.log.1");
        if previous.exists() {
            std::fs::remove_file(&previous)?;
        }
        std::fs::rename(&path, previous)?;
    }
    Ok(OpenOptions::new().create(true).append(true).open(path)?)
}

pub fn rotate_running_log(home: &Path) -> anyhow::Result<()> {
    let path = home.join("log/server.log");
    let Ok(metadata) = std::fs::metadata(&path) else {
        return Ok(());
    };
    if metadata.len() < 8 * 1024 * 1024 {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::{
            io::Write,
            os::{
                fd::{AsFd, AsRawFd},
                unix::fs::MetadataExt,
            },
        };
        let stdout = File::from(std::io::stdout().as_fd().try_clone_to_owned()?);
        let current = stdout.metadata()?;
        // Explicit foreground mode may still be attached to a terminal.
        if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
            return Ok(());
        }
        std::io::stdout().flush()?;
        std::io::stderr().flush()?;
        let log = log_file(home)?;
        for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            if unsafe { libc::dup2(log.as_raw_fd(), fd) } == -1 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
    }
    #[cfg(windows)]
    {
        // Preserve the inherited append handles while bounding the active file.
        std::fs::copy(&path, home.join("log/server.log.1"))?;
        OpenOptions::new().write(true).open(&path)?.set_len(0)?;
    }
    Ok(())
}

/// Exec preserves the launching thread's signal mask. Restore signals owned by
/// FAE before creating Tokio workers so every worker inherits a usable mask.
pub fn prepare_process_signals() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut signals = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut signals);
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGCHLD] {
                libc::sigaddset(&mut signals, signal);
            }
        }
        let result =
            unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &signals, std::ptr::null_mut()) };
        anyhow::ensure!(
            result == 0,
            "unblock process signals: {}",
            std::io::Error::from_raw_os_error(result)
        );
    }
    Ok(())
}

pub fn stop_signal() -> anyhow::Result<impl std::future::Future<Output = ()>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        Ok(async move {
            tokio::select! { _ = term.recv() => {}, _ = interrupt.recv() => {} }
        })
    }
    #[cfg(not(unix))]
    {
        Ok(async {
            let _ = tokio::signal::ctrl_c().await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lock_and_owned_discovery_cleanup_preserve_other_instances() {
        let home = std::env::temp_dir().join(format!("fae-lock-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(&home).unwrap();
        let lock = FileLock::try_acquire(&home.join("server.lock"))
            .unwrap()
            .unwrap();
        assert!(
            FileLock::try_acquire(&home.join("server.lock"))
                .unwrap()
                .is_none()
        );
        let config = ServerConfig {
            instance_id: "instance".into(),
            pid: std::process::id(),
            address: "127.0.0.1".parse().unwrap(),
            port: 1666,
            protocol_version: 1,
            app_version: "test".into(),
            started_at: "".into(),
            executable: "fae".into(),
        };
        publish(&home, &config, "private-token").unwrap();
        assert!(process_alive(config.pid).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(home.join("server.token"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        remove_owned(&home, "other").unwrap();
        assert!(home.join("server.config").exists());
        remove_owned(&home, "instance").unwrap();
        assert!(!home.join("server.config").exists());
        assert!(home.join("server.lock").exists());
        drop(lock);
        assert!(
            FileLock::try_acquire(&home.join("server.lock"))
                .unwrap()
                .is_some()
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
