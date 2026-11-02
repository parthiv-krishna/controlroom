//! What the shell configures, and the services every hub shares.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use controlroom_client_core::MasterKey;
use controlroom_client_core::bootstrap::DirBinaries;
use controlroom_client_core::ssh::ConnectOptions;
use controlroom_protocol::ids::ControllerId;

use crate::auth::AuthPolicy;
use crate::notify::PushConfig;
use crate::store::Store;

/// Everything the shell provides to [`crate::Controller::start`].
pub struct ControllerConfig {
    /// The SQLite client store (created if missing).
    pub store_path: PathBuf,
    /// Seals SSH private keys at rest. Required; the server never generates one.
    pub master_key: MasterKey,
    /// The bundled `controlroomd-<arch>-linux` binaries and their `version` file
    /// (`CONTROLROOM_BUNDLED_DAEMONS_DIR`). Without it, machines that already run a daemon
    /// still connect, but nothing can be installed or upgraded.
    pub bundled_daemons_dir: Option<PathBuf>,
    /// The server's version, reported in the UI `hello`.
    pub server_version: String,
    /// Who may sign in.
    pub auth: AuthPolicy,
    /// Web Push; `None` disables it.
    pub push: Option<PushConfig>,
    /// The comment of generated SSH keys, e.g. `controlroom@controlroom.example.com`.
    pub key_comment: String,
    /// Timeouts, backoff and limits.
    pub tuning: Tuning,
}

/// Timeouts, backoff and limits; the defaults suit a deployment.
#[derive(Debug, Clone)]
pub struct Tuning {
    /// SSH connect timeout and keepalives.
    pub ssh: ConnectOptions,
    /// Reconnect delays after a failed or lost connection.
    pub backoff: Backoff,
    /// The timeout of daemon calls that don't set their own.
    pub call_timeout: Duration,
    /// The timeout of slow daemon calls (`doctor.report`, `fs.list`).
    pub slow_call_timeout: Duration,
    /// How long to wait for a stopped daemon to exit during an upgrade.
    pub stop_wait: Duration,
    /// How long a machine must stay idle (no run in flight, no session changes) before it is
    /// upgraded automatically.
    pub auto_upgrade_quiet: Duration,
    /// Delays before retrying a failed automatic upgrade.
    pub auto_upgrade_retry: Backoff,
    /// Requests one UI connection may have running at once; the server stops reading the
    /// connection while at the cap.
    pub max_concurrent_requests: usize,
    /// Outbound messages buffered per UI connection; a client that falls further behind is
    /// closed and catches up after reconnecting.
    pub client_buffer: usize,
    /// How long a login session lasts without use (sliding).
    pub auth_session_ttl: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            ssh: ConnectOptions::default(),
            backoff: Backoff {
                initial: Duration::from_secs(1),
                max: Duration::from_secs(60),
            },
            call_timeout: Duration::from_secs(30),
            slow_call_timeout: Duration::from_secs(120),
            stop_wait: Duration::from_secs(10),
            auto_upgrade_quiet: Duration::from_secs(5),
            auto_upgrade_retry: Backoff {
                initial: Duration::from_secs(60),
                max: Duration::from_secs(60 * 60),
            },
            max_concurrent_requests: 8,
            client_buffer: 4096,
            auth_session_ttl: Duration::from_secs(30 * 24 * 60 * 60),
        }
    }
}

/// Exponential backoff: `initial`, doubling up to `max`.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    /// The first delay.
    pub initial: Duration,
    /// The longest delay.
    pub max: Duration,
}

impl Backoff {
    /// The delay before retry number `attempt` (from 0).
    pub fn delay(&self, attempt: u32) -> Duration {
        self.initial
            .saturating_mul(1u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX))
            .min(self.max)
    }
}

/// The services every hub shares.
pub(crate) struct Services {
    pub store: Store,
    pub master_key: MasterKey,
    /// The bundled daemons, or why there are none.
    pub binaries: Result<Arc<DirBinaries>, String>,
    pub server_version: String,
    pub controller_id: ControllerId,
    pub auth: AuthPolicy,
    pub push: Option<PushConfig>,
    pub key_comment: String,
    pub tuning: Tuning,
}

impl Services {
    /// The `client_name` in daemon `hello`s.
    pub(crate) fn client_name(&self) -> String {
        format!("controlroom-web {}", self.server_version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_max() {
        let b = Backoff {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(1),
        };
        let delays: Vec<_> = (0..6).map(|a| b.delay(a).as_millis()).collect();
        assert_eq!(delays, [100, 200, 400, 800, 1000, 1000]);
        assert_eq!(b.delay(u32::MAX), Duration::from_secs(1));
    }
}
