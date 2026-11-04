//! The single daemon instance per user: its lock, its socket, and starting it detached.
//!
//! - [`claim`] takes `<state>/run/daemon.lock` (an exclusive `flock`, released by the kernel
//!   when the daemon exits however it exits), replaces a stale socket, and binds
//!   `<state>/run/daemon.sock` (mode 0600). A second daemon finds the lock held and reports
//!   [`Claim::AlreadyRunning`].
//! - [`spawn_detached`] starts `<exe> serve --detached-child` in a new session (no controlling
//!   terminal, so no SIGHUP when the SSH channel closes) with stdin from `/dev/null`, stderr
//!   appended to `<state>/daemon.log`, and stdout as a pipe on which the child reports one
//!   [`ReadyLine`] once its socket is bound. The child's command line is
//!   `<…/controlroomd> serve …`, which the controller's stop script matches.
//! - [`serve_binary`] picks the executable to start: this process's own (the immutable
//!   `versions/<ver>/controlroomd` once installed; `/proc/self/exe` resolves the `current`
//!   symlink), after checking that it still reports this version.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use rustix::fs::{FlockOperation, flock};

use controlroom_sandbox::AbsPath;

use crate::dirs::{Dirs, create_private_dir};
use crate::version::version_line;

/// The hidden `serve` flag of the detached child.
pub const DETACHED_CHILD_FLAG: &str = "--detached-child";

/// A claimed daemon instance: holds the lock for as long as it lives.
pub struct Instance {
    _lock: File,
    /// The bound daemon socket.
    pub listener: UnixListener,
}

/// The result of [`claim`].
pub enum Claim {
    /// This process is the daemon.
    Acquired(Instance),
    /// Another daemon holds the lock.
    AlreadyRunning,
}

/// Takes the instance lock and binds the daemon socket.
pub fn claim(dirs: &Dirs) -> anyhow::Result<Claim> {
    claim_at(dirs, &dirs.daemon_lock(), &dirs.daemon_socket())
}

/// Takes `lock_path` and binds `socket` (both under `<state>/run`); the host role's instance
/// uses its own pair, so it runs alongside the session daemon.
pub fn claim_at(dirs: &Dirs, lock_path: &AbsPath, socket: &AbsPath) -> anyhow::Result<Claim> {
    let run = dirs.run();
    create_private_dir(&run).with_context(|| format!("creating {run}"))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path.as_path())
        .with_context(|| format!("opening {lock_path}"))?;
    match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(rustix::io::Errno::WOULDBLOCK) => return Ok(Claim::AlreadyRunning),
        Err(e) => return Err(e).with_context(|| format!("locking {lock_path}")),
    }
    // We hold the lock, so nobody serves a leftover socket file.
    match std::fs::remove_file(socket.as_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("removing the stale socket {socket}"));
        }
        _ => {}
    }
    let listener =
        UnixListener::bind(socket.as_path()).with_context(|| format!("binding {socket}"))?;
    std::fs::set_permissions(socket.as_path(), std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {socket}"))?;
    Ok(Claim::Acquired(Instance {
        _lock: lock,
        listener,
    }))
}

/// What a detached child reports on its stdout, once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadyLine {
    /// The socket is bound and accepting.
    Ready,
    /// Another daemon is already running; the child exited.
    AlreadyRunning,
    /// Startup failed.
    Error(String),
}

impl ReadyLine {
    /// The line, without its newline.
    pub fn encode(&self) -> String {
        match self {
            ReadyLine::Ready => "ready".to_owned(),
            ReadyLine::AlreadyRunning => "already-running".to_owned(),
            ReadyLine::Error(e) => format!("error: {}", e.replace('\n', " ")),
        }
    }

    /// Parses a line written by [`ReadyLine::encode`].
    pub fn parse(line: &str) -> Option<Self> {
        match line.trim_end() {
            "ready" => Some(ReadyLine::Ready),
            "already-running" => Some(ReadyLine::AlreadyRunning),
            other => other
                .strip_prefix("error: ")
                .map(|e| ReadyLine::Error(e.to_owned())),
        }
    }

    /// Writes the line to stdout (the launcher's pipe). A launcher that already gave up is
    /// not an error.
    pub fn report(&self) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", self.encode()).and_then(|()| out.flush());
    }
}

/// How a detached start ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    /// A new daemon is up.
    New,
    /// One was already running.
    AlreadyRunning,
}

/// Starts `exe serve` as a detached daemon and waits until its socket is bound.
pub fn spawn_detached(exe: &Path, dirs: &Dirs) -> anyhow::Result<Started> {
    spawn_detached_with(exe, dirs, &["serve"], &dirs.daemon_log())
}

/// Starts `exe <subcommand…> --state-dir <state> --detached-child` with stderr appended to
/// `log_path`, and waits for its ready line (the host role uses `host serve`).
pub fn spawn_detached_with(
    exe: &Path,
    dirs: &Dirs,
    subcommand: &[&str],
    log_path: &AbsPath,
) -> anyhow::Result<Started> {
    create_private_dir(dirs.state()).with_context(|| format!("creating {}", dirs.state()))?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log_path.as_path())
        .with_context(|| format!("opening {log_path}"))?;
    let mut child = Command::new(exe)
        .args(subcommand)
        .args(["--state-dir", dirs.state().as_str(), DETACHED_CHILD_FLAG])
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))?;
    let mut line = String::new();
    if let Some(stdout) = child.stdout.take() {
        BufReader::new(stdout).read_line(&mut line)?;
    }
    match ReadyLine::parse(&line) {
        Some(ReadyLine::Ready) => Ok(Started::New),
        Some(ReadyLine::AlreadyRunning) => {
            let _ = child.wait();
            Ok(Started::AlreadyRunning)
        }
        Some(ReadyLine::Error(e)) => {
            let _ = child.wait();
            bail!("the daemon failed to start: {e}")
        }
        None => {
            let status = child.wait()?;
            bail!("the daemon exited ({status}) before it was ready; see {log_path}")
        }
    }
}

/// The executable to start the daemon from; see the module docs.
pub fn serve_binary() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("resolving the controlroomd path")?;
    let out = Command::new(&exe)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {} --version", exe.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first = stdout.lines().next().unwrap_or_default();
    if !out.status.success() || first != version_line() {
        bail!(
            "{} reports {first:?}, expected {:?}; reinstall the daemon",
            exe.display(),
            version_line()
        );
    }
    Ok(exe)
}

/// Puts the detached child in a new session, away from the launcher's terminal and process
/// group.
pub fn detach_from_launcher() -> anyhow::Result<()> {
    rustix::process::setsid().context("setsid")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_lines_round_trip() {
        for line in [
            ReadyLine::Ready,
            ReadyLine::AlreadyRunning,
            ReadyLine::Error("bind failed".into()),
        ] {
            assert_eq!(ReadyLine::parse(&line.encode()), Some(line));
        }
        assert_eq!(ReadyLine::Error("a\nb".into()).encode(), "error: a b");
        assert_eq!(ReadyLine::parse("garbage"), None);
        assert_eq!(ReadyLine::parse(""), None);
    }

    #[test]
    fn one_instance_at_a_time() {
        let t = tempfile::tempdir().unwrap();
        let home = controlroom_sandbox::AbsPath::from_path(t.path()).unwrap();
        let dirs = Dirs::under_home(home);
        let Claim::Acquired(first) = claim(&dirs).unwrap() else {
            panic!("first claim must succeed");
        };
        assert!(matches!(claim(&dirs).unwrap(), Claim::AlreadyRunning));
        let mode = std::fs::metadata(dirs.daemon_socket().as_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // Re-claiming after the daemon exits (lock released, stale socket replaced) is tested
        // end to end in tests/connect.rs: in this process, a child forked by a concurrent test
        // could briefly hold a copy of the lock fd.
        drop(first);
    }
}
