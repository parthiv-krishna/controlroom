//! Spawning the child in its own process group, signaling the group, and reaping.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{Mutex, PoisonError};

use controlroom_transcript::StdinMode;
use rustix::io::Errno;
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};

/// A spawned child and its stdio pipes.
pub(crate) struct Spawned {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

/// Spawns `argv` as the leader of a new process group.
pub(crate) fn spawn(
    argv: &[String],
    cwd: &Path,
    env: Vec<(String, String)>,
    stdin: StdinMode,
) -> io::Result<Spawned> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
    let stdin_cfg = match stdin {
        StdinMode::Null => Stdio::null(),
        StdinMode::Pipe | StdinMode::Unknown => Stdio::piped(),
    };
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .envs(env)
        .stdin(stdin_cfg)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let stdout = child.stdout.take().ok_or_else(|| missing("stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| missing("stderr"))?;
    Ok(Spawned {
        stdin: child.stdin.take(),
        stdout,
        stderr,
        child,
    })
}

fn missing(what: &str) -> io::Error {
    io::Error::other(format!("child {what} is not piped"))
}

/// The child's process group (its id is the child's pid).
///
/// Signals are only sent while the leader is unreaped: until then its pid, and so
/// the group id, cannot be reused by an unrelated process.
pub(crate) struct Group {
    pgid: u32,
    reaped: Mutex<bool>,
}

impl Group {
    pub(crate) fn new(pgid: u32) -> Self {
        Self {
            pgid,
            reaped: Mutex::new(false),
        }
    }

    /// Sends `signal` to every process in the group, unless the leader is reaped.
    pub(crate) fn signal(&self, signal: Signal) {
        let reaped = self.reaped.lock().unwrap_or_else(PoisonError::into_inner);
        if !*reaped {
            signal_group(self.pgid, signal);
        }
    }

    /// Blocks until the leader exits, SIGKILLs whatever is left in its group (no
    /// leaked children), then reaps the leader.
    pub(crate) fn wait_and_reap(&self, child: &mut Child) -> io::Result<ExitStatus> {
        wait_for_exit(self.pgid)?;
        let mut reaped = self.reaped.lock().unwrap_or_else(PoisonError::into_inner);
        signal_group(self.pgid, Signal::KILL);
        let status = child.wait();
        *reaped = true;
        status
    }
}

/// Waits for `pid` to exit without reaping it.
fn wait_for_exit(pid: u32) -> io::Result<()> {
    let pid = to_pid(pid).ok_or_else(|| io::Error::other("invalid child pid"))?;
    loop {
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        ) {
            Ok(_) => return Ok(()),
            Err(Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
    }
}

fn to_pid(pid: u32) -> Option<Pid> {
    // Pid 1 would mean "every process we may signal" for kill(-1).
    i32::try_from(pid)
        .ok()
        .filter(|&p| p > 1)
        .and_then(Pid::from_raw)
}

/// `kill(-pgid, signal)`, ignoring groups that are already gone.
fn signal_group(pgid: u32, signal: Signal) {
    let Some(pid) = to_pid(pgid) else {
        return;
    };
    if let Err(e) = kill_process_group(pid, signal)
        && e != Errno::SRCH
    {
        eprintln!("supervise: kill(-{pgid}, {signal:?}): {e}");
    }
}
