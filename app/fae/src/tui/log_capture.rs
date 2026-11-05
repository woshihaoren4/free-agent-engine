use std::{io, sync::mpsc::{self, Receiver}};
#[cfg(unix)]
use std::{fs::File, io::{BufRead, BufReader, Write}, os::fd::{AsRawFd, FromRawFd, OwnedFd}};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LogStream { Stdout, Stderr }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LogKind { Output, Error }
#[derive(Debug)]
pub(super) struct CapturedLog {
    pub(super) kind: LogKind,
    pub(super) content: String,
}
pub(super) struct LogCapture {
    pub(super) receiver: Receiver<CapturedLog>,
    #[cfg(unix)]
    _stdout_writer: OwnedFd,
    #[cfg(unix)]
    _stderr_writer: OwnedFd,
    #[cfg(unix)]
    original_stdout: Option<OwnedFd>,
    #[cfg(unix)]
    original_stderr: Option<OwnedFd>,
}
impl LogCapture {
    #[cfg(unix)]
    pub(super) fn new() -> io::Result<Self> {
        io::stdout().flush()?;
        io::stderr().flush()?;
        let original_stdout = duplicate_fd(libc::STDOUT_FILENO)?;
        let original_stderr = duplicate_fd(libc::STDERR_FILENO)?;
        let (stdout_reader, stdout_writer) = create_pipe()?;
        let (stderr_reader, stderr_writer) = create_pipe()?;
        if unsafe { libc::dup2(stdout_writer.as_raw_fd(), libc::STDOUT_FILENO) } == -1 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::dup2(stderr_writer.as_raw_fd(), libc::STDERR_FILENO) } == -1 {
            let error = io::Error::last_os_error();
            unsafe { libc::dup2(original_stdout.as_raw_fd(), libc::STDOUT_FILENO); }
            return Err(error);
        }
        let (sender, receiver) = mpsc::channel();
        spawn_log_reader(stdout_reader, sender.clone(), LogStream::Stdout);
        spawn_log_reader(stderr_reader, sender, LogStream::Stderr);
        Ok(Self { receiver, _stdout_writer: stdout_writer, _stderr_writer: stderr_writer,
            original_stdout: Some(original_stdout), original_stderr: Some(original_stderr) })
    }
    #[cfg(not(unix))]
    pub(super) fn new() -> io::Result<Self> {
        let (_sender, receiver) = mpsc::channel();
        Ok(Self { receiver })
    }
}
#[cfg(unix)]
fn duplicate_fd(fd: libc::c_int) -> io::Result<OwnedFd> {
    let duplicated = unsafe { libc::dup(fd) };
    if duplicated == -1 { Err(io::Error::last_os_error()) }
    else { Ok(unsafe { OwnedFd::from_raw_fd(duplicated) }) }
}
#[cfg(unix)]
fn create_pipe() -> io::Result<(File, OwnedFd)> {
    let mut pipe_fds = [-1; 2];
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } == -1 { return Err(io::Error::last_os_error()); }
    Ok(unsafe { (File::from_raw_fd(pipe_fds[0]), OwnedFd::from_raw_fd(pipe_fds[1])) })
}
#[cfg(unix)]
fn spawn_log_reader(reader: File, sender: mpsc::Sender<CapturedLog>, stream: LogStream) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let content = String::from_utf8_lossy(&line);
                    let kind = captured_log_kind(stream, &content);
                    let content = strip_ansi(&content).trim_end_matches(['\r','\n']).to_string();
                    if !content.is_empty() && sender.send(CapturedLog { kind, content }).is_err() { break; }
                }
            }
        }
    });
}
pub(super) fn captured_log_kind(stream: LogStream, content: &str) -> LogKind {
    if stream == LogStream::Stderr || content.contains("\u{1b}[7;31m") { LogKind::Error } else { LogKind::Output }
}
pub(super) fn strip_ansi(content: &str) -> String {
    let mut stripped = String::with_capacity(content.len());
    let mut characters = content.chars();
    while let Some(character) = characters.next() {
        if character != '\u{1b}' { stripped.push(character); continue; }
        if characters.next() == Some('[') {
            for character in characters.by_ref() { if ('@'..='~').contains(&character) { break; } }
        }
    }
    stripped
}
#[cfg(unix)]
impl Drop for LogCapture {
    fn drop(&mut self) {
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
        if let Some(fd) = self.original_stdout.take() { unsafe { libc::dup2(fd.as_raw_fd(), libc::STDOUT_FILENO); } }
        if let Some(fd) = self.original_stderr.take() { unsafe { libc::dup2(fd.as_raw_fd(), libc::STDERR_FILENO); } }
    }
}
