//! The single agent daemon instance: its lock and its socket.
//!
//! [`claim`] takes `<state>/run/agentd.lock` (an exclusive `flock`, released by the kernel when
//! the daemon exits however it exits), replaces a stale socket, and binds the agent daemon API
//! socket (mode 0660: the controller reaches it through a shared group). A second daemon finds
//! the lock held and fails.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;

use anyhow::{Context, bail};
use rustix::fs::{FlockOperation, flock};

use crate::dirs::{Dirs, create_private_dir};

/// A claimed instance: holds the lock for as long as it lives.
pub struct Instance {
    _lock: File,
    /// The bound agent daemon API socket.
    pub listener: UnixListener,
}

/// Takes the instance lock and binds the socket.
pub fn claim(dirs: &Dirs) -> anyhow::Result<Instance> {
    let run = dirs.run();
    create_private_dir(&run).with_context(|| format!("creating {run}"))?;
    let lock_path = dirs.lock();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path.as_path())
        .with_context(|| format!("opening {lock_path}"))?;
    match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(rustix::io::Errno::WOULDBLOCK) => {
            bail!("another agent daemon is running ({lock_path})")
        }
        Err(e) => return Err(e).with_context(|| format!("locking {lock_path}")),
    }
    let socket = dirs.socket();
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent.as_path()).with_context(|| format!("creating {parent}"))?;
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
    std::fs::set_permissions(socket.as_path(), std::fs::Permissions::from_mode(0o660))
        .with_context(|| format!("restricting {socket}"))?;
    Ok(Instance {
        _lock: lock,
        listener,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_instance_at_a_time() {
        let t = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(controlroom_sandbox::AbsPath::from_path(t.path()).unwrap());
        let first = claim(&dirs).unwrap();
        assert!(claim(&dirs).is_err());
        let mode = std::fs::metadata(dirs.socket().as_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o660);
        drop(first);
    }
}
