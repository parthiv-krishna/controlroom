//! The full install/upgrade sequence with rollback.

use std::fmt::Display;
use std::future::Future;
use std::time::Duration;

use super::binaries::{DaemonBinarySource, Program};
use super::install::{InstallStep, Installed, install, rollback};
use super::platform::InstallDecision;
use super::probe::probe;
use super::stop::stop_daemon;
use super::{BootstrapError, RemoteShell};

/// What [`upgrade`] did.
#[derive(Debug)]
pub enum UpgradeOutcome<T> {
    /// The machine already runs the available version; nothing changed.
    UpToDate,
    /// The new version is installed, current, and passed the check.
    Upgraded {
        /// The install, including the previous `current` target (boxed: it is large).
        installed: Box<Installed>,
        /// How many old daemons were stopped.
        stopped: u32,
        /// The check's result (e.g. the new daemon's `hello`).
        checked: T,
    },
}

/// Installs or upgrades the daemon, then proves the new one works:
///
/// 1. probe the machine; refuse to downgrade ([`BootstrapError::WouldDowngrade`]);
/// 2. [`install`]: upload (nono only if the machine lacks its version), verify (hash and
///    verify-run), swap `current`; `progress` hears about each upload;
/// 3. stop the old daemon and confirm it exited (within `stop_wait`);
/// 4. run `check`, which should connect to the new daemon and complete `hello`.
///
/// If step 3 or 4 fails, any new daemon is stopped and `current` is rolled
/// back to its previous target ([`BootstrapError::RolledBack`]), so the next
/// connect runs the old daemon again. A failed verification never touches
/// `current`.
pub async fn upgrade<S, B, F, Fut, T, E>(
    shell: &S,
    source: &B,
    progress: &(dyn Fn(InstallStep<'_>) + Sync),
    stop_wait: Duration,
    check: F,
) -> Result<UpgradeOutcome<T>, BootstrapError>
where
    S: RemoteShell,
    B: DaemonBinarySource + ?Sized,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: Display,
{
    let probe = probe(shell).await?;
    if probe.decide(source.version(Program::Controlroomd)) == InstallDecision::UpToDate {
        return Ok(UpgradeOutcome::UpToDate);
    }
    let installed = install(shell, source, &probe, progress).await?;
    let stopped = match stop_daemon(shell, stop_wait).await {
        Ok(stopped) => stopped,
        Err(e) => return Err(undo(shell, &installed, stop_wait, e.to_string()).await),
    };
    match check().await {
        Ok(checked) => Ok(UpgradeOutcome::Upgraded {
            installed: Box::new(installed),
            stopped,
            checked,
        }),
        Err(e) => Err(undo(shell, &installed, stop_wait, e.to_string()).await),
    }
}

/// Stops whatever daemon runs now and rolls `current` back.
///
/// The rollback happens even if a daemon won't stop: when the old daemon
/// ignored SIGTERM, pointing `current` back at it is exactly right. A daemon
/// still running is added to the reason.
async fn undo<S: RemoteShell>(
    shell: &S,
    installed: &Installed,
    stop_wait: Duration,
    mut reason: String,
) -> BootstrapError {
    tracing::warn!(version = %installed.version, %reason, "rolling back the daemon upgrade");
    if let Err(e) = stop_daemon(shell, stop_wait).await {
        reason = format!("{reason}; while rolling back: {e}");
    }
    match rollback(shell, installed).await {
        Ok(restored) => BootstrapError::RolledBack { reason, restored },
        Err(e) => BootstrapError::RollbackFailed {
            reason,
            rollback: Box::new(e),
        },
    }
}
