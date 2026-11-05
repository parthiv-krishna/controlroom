//! Probe, install, upgrade and roll back `controlroomd` (with the nono it sandboxes
//! workspaces with) on a machine, and edit `authorized_keys` for key rotation.
//!
//! Remote layout, under `~/.local/share/controlroom`:
//!
//! ```text
//! versions/<ver>/controlroomd   immutable; supervisors keep running from it
//! versions/<ver>/nono -> ../../nono/<nono ver>/nono   the daemon's nono
//! nono/<nono ver>/nono          immutable; keyed by nono's version, so daemon upgrades
//!                               with the same nono upload nothing for it
//! current -> versions/<ver>     swapped atomically on install and rollback
//! ```
//!
//! Every remote command is a POSIX `sh` script wrapped with
//! [`crate::shell::sh_c`], whatever the user's login shell is. Remote paths
//! are POSIX strings, never `std::path`.

pub mod authorized_keys;
mod binaries;
mod install;
mod platform;
mod probe;
mod prune;
mod rotate;
mod stop;
mod upgrade;

use std::future::Future;

pub use authorized_keys::{add_authorized_key, remove_authorized_key};
pub use binaries::{
    DaemonBinarySource, DirBinaries, InMemoryBinaries, InMemoryProgram, Program,
    bundled_binary_name,
};
pub use install::{InstallStep, Installed, InstalledNono, install, read_current_link, rollback};
pub use platform::{
    Arch, InstallDecision, UnsupportedPlatform, Version, decide_install, parse_uname,
    parse_version_output,
};
pub use probe::{ProbeResult, probe};
pub use prune::{Pruned, prune_versions};
pub use rotate::rotate_key;
pub use stop::stop_daemon;
pub use upgrade::{UpgradeOutcome, upgrade};

use crate::shell::{quote, sh_c};
use crate::ssh::{CommandOutput, ExitStatus, SshConnection, SshError};

/// `controlroomd`'s data directory, relative to `$HOME` on the remote machine.
pub const REMOTE_DATA_DIR: &str = ".local/share/controlroom";
/// The stable daemon path, relative to `$HOME`.
pub const REMOTE_CURRENT_BINARY: &str = ".local/share/controlroom/current/controlroomd";

/// Shell prelude shared by the scripts: `$base`, a bounded `--version` run, and whether
/// `--version` output (`controlroomd 0.1.0`, `nono 0.80.0`) reports a version.
const PRELUDE: &str = r#"base="$HOME/.local/share/controlroom"
version_of() {
  if command -v timeout >/dev/null 2>&1; then timeout 10 "$1" --version; else "$1" --version; fi
}
reports_version() {
  case "$(printf '%s\n' "$1" | head -n 1)" in
    *" $2"|*" v$2") return 0 ;;
    *) return 1 ;;
  esac
}
"#;

/// The exec command that runs the current daemon with `args`, e.g.
/// `["connect"]` for the JSON-RPC bridge.
pub fn daemon_command(args: &[&str]) -> String {
    let mut script = format!("exec \"$HOME/{REMOTE_CURRENT_BINARY}\"");
    for arg in args {
        script.push(' ');
        script.push_str(&quote(arg));
    }
    sh_c(&script)
}

/// Runs a command on a machine. Implemented by [`SshConnection`]; tests can
/// run commands locally instead.
pub trait RemoteShell: Sync {
    /// Runs `command` to completion, feeding it `stdin`.
    fn run(
        &self,
        command: &str,
        stdin: &[u8],
    ) -> impl Future<Output = Result<CommandOutput, SshError>> + Send;
}

impl RemoteShell for SshConnection {
    fn run(
        &self,
        command: &str,
        stdin: &[u8],
    ) -> impl Future<Output = Result<CommandOutput, SshError>> + Send {
        SshConnection::run(self, command, stdin)
    }
}

/// Why a bootstrap step failed.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// The SSH transport failed.
    #[error(transparent)]
    Ssh(#[from] SshError),
    /// The machine isn't Linux x86_64 or aarch64.
    #[error(transparent)]
    UnsupportedPlatform(#[from] UnsupportedPlatform),
    /// A remote script failed.
    #[error("{what} failed ({status}): {stderr}")]
    CommandFailed {
        /// Which step.
        what: &'static str,
        /// How the script ended.
        status: ExitStatus,
        /// Its stderr.
        stderr: String,
    },
    /// An uploaded binary was truncated, corrupt, or failed its verify-run; it was removed
    /// and `current` was not touched.
    #[error("the new {program} failed verification: {detail}")]
    Verification {
        /// Which binary.
        program: Program,
        /// What failed.
        detail: String,
    },
    /// No binary is available for the machine's architecture.
    #[error("no {program} binary for {arch}: {source}")]
    MissingBinary {
        /// Which program.
        program: Program,
        /// The machine's architecture.
        arch: Arch,
        /// Why the source couldn't provide it.
        #[source]
        source: std::io::Error,
    },
    /// The machine runs a newer daemon than the one available. Never downgrade.
    #[error("refusing to downgrade controlroomd {installed} to {available}")]
    WouldDowngrade {
        /// The installed version.
        installed: Version,
        /// The version that would have been installed.
        available: Version,
    },
    /// Daemon processes kept running after SIGTERM.
    #[error("controlroomd is still running after SIGTERM (pids {pids:?})")]
    DaemonStillRunning {
        /// The processes that didn't exit.
        pids: Vec<u32>,
    },
    /// The new daemon was activated but didn't come up; `current` points back
    /// at `restored` (or was removed if there was no previous daemon).
    #[error("the new daemon failed ({reason}); rolled back to {}", restored.as_deref().unwrap_or("no daemon"))]
    RolledBack {
        /// Why the new daemon was rejected.
        reason: String,
        /// The `current` target restored, e.g. `versions/0.1.0`.
        restored: Option<String>,
    },
    /// The new daemon didn't come up, and rolling back failed too.
    #[error("the new daemon failed ({reason}), and rolling back failed: {rollback}")]
    RollbackFailed {
        /// Why the new daemon was rejected.
        reason: String,
        /// Why the rollback failed.
        rollback: Box<BootstrapError>,
    },
    /// A remote script printed something unexpected.
    #[error("unexpected remote output: {0}")]
    Parse(String),
}

/// Runs `script` under `sh` with [`PRELUDE`] and fails with its stderr unless it exits 0.
async fn run_script<S: RemoteShell>(
    shell: &S,
    what: &'static str,
    script: &str,
    stdin: &[u8],
) -> Result<CommandOutput, BootstrapError> {
    let out = shell
        .run(&sh_c(&format!("{PRELUDE}{script}")), stdin)
        .await?;
    if !out.status.success() {
        return Err(BootstrapError::CommandFailed {
            what,
            stderr: out.stderr_str().trim().to_owned(),
            status: out.status,
        });
    }
    Ok(out)
}

/// The values of every `key=value` line for `key`, in order.
fn fields<'a>(output: &'a str, key: &'a str) -> impl Iterator<Item = &'a str> {
    output.lines().filter_map(move |line| {
        line.split_once('=')
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v.trim())
    })
}

/// The value of the first `key=value` line for `key`.
fn field<'a>(output: &'a str, key: &'a str) -> Option<&'a str> {
    fields(output, key).next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_command_quotes_args() {
        assert_eq!(
            daemon_command(&["pty", "attach", "a b"]),
            sh_c(
                r#"exec "$HOME/.local/share/controlroom/current/controlroomd" 'pty' 'attach' 'a b'"#
            )
        );
    }

    #[test]
    fn key_value_fields() {
        let out = "a=1\nb= 2 \nnoise\na=3\nab=4\n";
        assert_eq!(field(out, "a"), Some("1"));
        assert_eq!(field(out, "b"), Some("2"));
        assert_eq!(fields(out, "a").collect::<Vec<_>>(), ["1", "3"]);
        assert_eq!(field(out, "c"), None);
    }
}
