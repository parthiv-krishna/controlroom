//! A machine as the hub sees it: its record, live status, daemon link, cached sessions and
//! supervising task.

use std::collections::BTreeMap;
use std::sync::Arc;

use controlroom_client_core::daemon::DaemonClient;
use controlroom_client_core::ssh::{HostKey, SshConnection};
use controlroom_protocol::agentd::HostMachine;
use controlroom_protocol::ids::{Epoch, KeyId, SessionId, SubscriptionId};
use controlroom_protocol::session::Availability;
use controlroom_protocol::session::{SessionState, SessionSummary};
use controlroom_protocol::ui::{BootstrapProgress, Connectivity, HostKeyInfo, Machine};
use controlroom_protocol::{Rev, UnixMillis};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::machines::MachineCommand;
use crate::store::MachineRow;

/// Which API a machine's daemon speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkKind {
    /// The host API (`controlroomd` in the host role): requests come from the agent daemon
    /// through the host router.
    Host {
        /// The host daemon's lifetime, from its `hello`.
        epoch: Epoch,
    },
    /// The deprecated per-machine daemon API (sessions on the machine; old UI API until R2).
    Legacy,
}

/// A live daemon connection.
#[derive(Clone)]
pub(crate) struct Link {
    pub daemon: DaemonClient,
    pub ssh: Arc<SshConnection>,
    /// The key this connection authenticated with.
    pub auth_key_id: KeyId,
    /// The API it speaks.
    pub kind: LinkKind,
}

/// Live status beyond the stored record.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MachineLive {
    pub connectivity: Connectivity,
    pub update_available: bool,
    pub last_error: Option<String>,
    pub bootstrap: Option<BootstrapProgress>,
    /// While connected: since when (a new value means a new connection). Otherwise: since
    /// the last disconnect (or the controller start), kept across reconnect attempts.
    pub since: UnixMillis,
}

impl Default for MachineLive {
    fn default() -> Self {
        Self {
            connectivity: Connectivity::Connecting,
            update_available: false,
            last_error: None,
            bootstrap: None,
            since: UnixMillis::now(),
        }
    }
}

impl MachineLive {
    /// Moves `since` when the machine connects or stops being connected.
    pub(crate) fn track_since(&mut self, before: &Connectivity) {
        let connected = Connectivity::Connected;
        if (*before == connected) != (self.connectivity == connected) {
            self.since = UnixMillis::now();
        }
    }
}

/// One machine in the hub.
pub(crate) struct MachineEntry {
    pub record: MachineRow,
    pub live: MachineLive,
    pub link: Option<Link>,
    /// The current `machine.subscribe` subscription on the link.
    pub machine_sub: Option<SubscriptionId>,
    /// The last machine-feed revision applied; resumes the feed after a reconnect.
    pub rev: Option<Rev>,
    /// The last known summaries; kept (stale) while the machine is unreachable.
    pub sessions: BTreeMap<SessionId, SessionSummary>,
    pub control: mpsc::Sender<MachineCommand>,
    pub task: Option<JoinHandle<()>>,
    /// Serializes session subscription changes on this machine.
    pub sub_lock: Arc<tokio::sync::Mutex<()>>,
    /// The host daemon's epoch from its latest `hello` (kept while disconnected).
    pub epoch: Option<Epoch>,
}

/// Whether a machine is due for an automatic upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpgradeCheck {
    /// Auto-upgrade is off, nothing newer is bundled, or the machine isn't connected.
    Off,
    /// Due, but a run is in flight.
    Busy,
    /// Due and idle.
    Idle,
}

/// A run is starting, active, or waiting on the user mid-run.
fn run_in_flight(session: &SessionSummary) -> bool {
    matches!(
        session.state,
        SessionState::Starting
            | SessionState::Working
            | SessionState::AwaitingPermission
            | SessionState::AwaitingInput
    )
}

pub(crate) fn host_key_info(key: &HostKey) -> HostKeyInfo {
    HostKeyInfo {
        algorithm: key.algorithm(),
        fingerprint: key.fingerprint(),
    }
}

impl MachineEntry {
    /// Sessions are current only while the machine is connected.
    pub(crate) fn stale(&self) -> bool {
        self.live.connectivity != Connectivity::Connected
    }

    /// Only a connected daemon older than the bundled version qualifies (`update_available`
    /// is never set for a newer one), so this never downgrades.
    pub(crate) fn upgrade_check(&self, host_calls: bool) -> UpgradeCheck {
        let due = self.record.auto_upgrade
            && self.live.update_available
            && self.live.connectivity == Connectivity::Connected;
        if !due {
            UpgradeCheck::Off
        } else if host_calls || self.sessions.values().any(run_in_flight) {
            UpgradeCheck::Busy
        } else {
            UpgradeCheck::Idle
        }
    }

    /// The protocol view.
    pub(crate) fn view(&self) -> Machine {
        let r = &self.record;
        Machine {
            id: r.id,
            name: r.name.clone(),
            host: r.host.clone(),
            port: r.port,
            user: r.username.clone(),
            key_id: r.key_id,
            host_key: r
                .host_key
                .as_deref()
                .and_then(|k| HostKey::parse(k).ok())
                .map(|k| host_key_info(&k)),
            daemon_id: r.daemon_id,
            connectivity: self.live.connectivity.clone(),
            update_available: self.live.update_available,
            auto_upgrade: r.auto_upgrade,
            grace_s: r.grace_s,
            last_seen: r.last_seen,
            version: r.version.clone(),
            arch: r.arch.clone(),
            last_error: self.live.last_error.clone(),
            capabilities: r.capabilities.clone(),
            settings: r.daemon_settings.clone(),
            doctor: r.doctor.clone(),
            bootstrap: self.live.bootstrap.clone(),
        }
    }
}

impl MachineEntry {
    /// The machine as the agent daemon sees it. A legacy daemon can't serve host requests, so
    /// it counts as `incompatible`.
    pub(crate) fn host_machine(&self) -> HostMachine {
        let legacy = self
            .link
            .as_ref()
            .is_some_and(|l| l.kind == LinkKind::Legacy);
        HostMachine {
            machine_id: self.record.id,
            name: self.record.name.clone(),
            availability: Availability {
                connectivity: if legacy {
                    Connectivity::Incompatible
                } else {
                    self.live.connectivity.clone()
                },
                since: self.live.since,
            },
            epoch: self.epoch,
            grace_s: self.record.grace_s,
        }
    }
}
