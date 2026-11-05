use std::path::{Path, PathBuf};
use anyhow::Context;

pub fn expand_home(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else { return path };
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else { return path };
    if text == "~" { return PathBuf::from(home); }
    text.strip_prefix("~/").map(|rest| PathBuf::from(home).join(rest)).unwrap_or(path)
}

pub fn absolute(path: PathBuf) -> anyhow::Result<PathBuf> {
    let path = expand_home(path);
    Ok(if path.is_absolute() { path } else { std::env::current_dir()?.join(path) })
}

pub fn workspace(path: PathBuf) -> anyhow::Result<PathBuf> {
    let path = absolute(path)?.canonicalize().context("resolve workspace")?;
    anyhow::ensure!(path.is_dir(), "workspace is not a directory: {}", path.display());
    Ok(path)
}

pub fn home(path: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let path = absolute(path.unwrap_or_else(fae_engine::default_fae_host))?;
    if !path.exists() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)] {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).with_context(|| format!("create home {}", path.display()))?;
    }
    path.canonicalize().context("resolve FAE home")
}

pub fn attachment(path: &str) -> anyhow::Result<String> {
    if path.starts_with("https://") || path.starts_with("http://") || path.starts_with("data:") {
        return Ok(path.into());
    }
    Ok(absolute(Path::new(path).to_owned())?.canonicalize()?.to_string_lossy().into_owned())
}
