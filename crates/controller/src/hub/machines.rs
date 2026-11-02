//! A machine as the hub sees it: its record, live status, daemon link, cached sessions and
//! supervising task.

use std::collections::BTreeMap;
use std::sync::Arc;

use controlroom_client_core::daemon::DaemonClient;
use controlroom_client_core::ssh::{HostKey, SshConnection};
use controlroom_protocol::Rev;
use controlroom_protocol::ids::{KeyId, SessionId, SubscriptionId};
use controlroom_protocol::session::{SessionState, SessionSummary};
use controlroom_protocol::ui::{BootstrapProgress, Connectivity, HostKeyInfo, Machine};
use controlroom_protocol::version::Capability;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::machines::MachineCommand;
use crate::store::MachineRow;

/// A live daemon connection.
#[derive(Clone)]
pub(crate) struct Link {
    pub daemon: DaemonClient,
    pub ssh: Arc<SshConnection>,
    /// The key this connection authenticated with.
    pub auth_key_id: KeyId,
}

/// Live status beyond the stored record.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MachineLive {
    pub connectivity: Connectivity,
    pub update_available: bool,
    pub last_error: Option<String>,
    pub bootstrap: Option<BootstrapProgress>,
    pub capabilities: Vec<Capability>,
}

impl Default for MachineLive {
    fn default() -> Self {
        Self {
            connectivity: Connectivity::Connecting,
            update_available: false,
            last_error: None,
            bootstrap: None,
            capabilities: Vec::new(),
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
    pub(crate) fn upgrade_check(&self) -> UpgradeCheck {
        let due = self.record.auto_upgrade
            && self.live.update_available
            && self.live.connectivity == Connectivity::Connected;
        if !due {
            UpgradeCheck::Off
        } else if self.sessions.values().any(run_in_flight) {
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
            last_seen: r.last_seen,
            version: r.version.clone(),
            arch: r.arch.clone(),
            last_error: self.live.last_error.clone(),
            capabilities: self.live.capabilities.clone(),
            settings: r.daemon_settings.clone(),
            doctor: r.doctor.clone(),
            bootstrap: self.live.bootstrap.clone(),
        }
    }
}
