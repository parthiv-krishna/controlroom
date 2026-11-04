//! Terminals (`kind: pty`): spawning the child on a fresh PTY, logging what it prints as byte
//! records, writing raw input, and resizing.
//!
//! The child becomes a session leader with the PTY's slave side as its controlling terminal,
//! so an interactive shell gets job control and the line discipline turns ETX into SIGINT for
//! the foreground process group. The supervisor keeps only the master side. Input is not
//! logged: what a user types (a password at a prompt) must not end up on disk.

use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use controlroom_transcript::PtyChunk;
use rustix::fs::{Mode, OFlags};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{Winsize, tcsetwinsize};

use super::log::{AppendError, Log};
use crate::diag::diag;
use crate::proto::{ErrorBody, ErrorCode};

/// The terminal size a PTY starts with, until a client resizes it.
pub(crate) const DEFAULT_SIZE: (u16, u16) = (120, 40);

/// Largest chunk logged as one record.
const CHUNK: usize = 64 * 1024;

/// A child running on a PTY, and the master side.
pub(crate) struct PtySpawned {
    pub child: Child,
    pub master: OwnedFd,
}

/// Spawns `argv` on a new PTY of the default size.
pub(crate) fn spawn(
    argv: &[String],
    cwd: &Path,
    env: Vec<(String, String)>,
) -> io::Result<PtySpawned> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "empty argv"))?;
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    resize(&master, DEFAULT_SIZE.0, DEFAULT_SIZE.1)?;
    let name = ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .envs(env)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // SAFETY: the hook runs in the forked child before exec and only makes two raw syscalls
    // (setsid, ioctl TIOCSCTTY on fd 0), both async-signal-safe; it allocates nothing and
    // touches no locks.
    #[allow(unsafe_code)]
    unsafe {
        command.pre_exec(|| {
            rustix::process::setsid()?;
            rustix::process::ioctl_tiocsctty(rustix::stdio::stdin())?;
            Ok(())
        });
    }
    let child = command.spawn()?;
    // `command` (and with it every copy of the slave side) is dropped here: once the child
    // and its descendants close theirs, reading the master ends with EIO.
    Ok(PtySpawned { child, master })
}

/// Sets the terminal size (the child gets SIGWINCH).
pub(crate) fn resize(master: &OwnedFd, cols: u16, rows: u16) -> io::Result<()> {
    tcsetwinsize(
        master,
        Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )?;
    Ok(())
}

/// Logs everything read from the master side as `pty` records until the slave side is closed
/// everywhere (EIO) or the log is closed. Runs on its own thread.
pub(crate) fn pump(master: File, log: &Arc<Log>) {
    let mut master = master;
    let mut buf = vec![0; CHUNK];
    loop {
        let n = match master.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            // EIO: every slave descriptor is closed.
            Err(e) if e.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => return,
            Err(e) => {
                diag!("reading the pty: {e}");
                return;
            }
        };
        let body = PtyChunk(buf[..n].to_vec()).into_body();
        match log.append(body) {
            Ok(_) => {}
            Err(AppendError::Closed) => return,
            Err(e) => diag!("logging pty output failed, dropping {n} bytes: {e}"),
        }
    }
}

/// Input for the PTY and where to report that it was written.
pub(crate) type Input = (Vec<u8>, tokio::sync::oneshot::Sender<Result<(), ErrorBody>>);

/// Writes queued input to the master side, in order; ends when every sender is gone.
pub(crate) fn writer(master: File, input: &Receiver<Input>) {
    let mut master = master;
    for (bytes, reply) in input {
        let result = master
            .write_all(&bytes)
            .map_err(|e| ErrorBody::new(ErrorCode::Io, format!("writing to the pty: {e}")));
        let _ = reply.send(result);
    }
}
