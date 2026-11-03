//! Running a credential command with a deadline and captured output.

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// What a finished command printed.
pub(super) struct Captured {
    /// Exit status 0.
    pub success: bool,
    /// Stdout, lossily decoded.
    pub stdout: String,
    /// Stderr, lossily decoded.
    pub stderr: String,
}

/// Why a command produced no [`Captured`].
#[derive(Debug)]
pub(super) enum RunError {
    /// It could not be started.
    Spawn(std::io::Error),
    /// It didn't finish in time and was killed.
    TimedOut,
}

/// Runs `command` with stdin closed and both outputs captured, killing it after `timeout`.
pub(super) fn run(mut command: Command, timeout: Duration) -> Result<Captured, RunError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::Spawn)?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::TimedOut);
            }
        }
    };
    Ok(Captured {
        success: status.success(),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Reads a pipe to the end on a thread, so a chatty child can't block on a full pipe.
fn drain(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}
