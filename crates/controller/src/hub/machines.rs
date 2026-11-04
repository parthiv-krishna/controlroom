//! A machine as the hub sees it: its record, live status, host daemon link and supervising
//! task.

use std::sync::Arc;

use controlroom_client_core::daemon::DaemonClient;
use controlroom_client_core::ssh::{HostKey, SshConnection};
use controlroom_protocol::UnixMillis;
use controlroom_protocol::agentd::HostMachine;
use controlroom_protocol::ids::{Epoch, KeyId};
use controlroom_protocol::session::Availability;
use controlroom_protocol::ui::{BootstrapProgress, Connectivity, HostKeyInfo, Machine};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::machines::MachineCommand;
use crate::store::MachineRow;

/// A live host daemon connection.
#[derive(Clone)]
pub(crate) struct Link {
    pub daemon: DaemonClient,
    pub ssh: Arc<SshConnection>,
    /// The key this connection authenticated with.
    pub auth_key_id: KeyId,
    /// The host daemon's lifetime, from its `hello`.
    pub epoch: Epoch,
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
    pub control: mpsc::Sender<MachineCommand>,
    pub task: Option<JoinHandle<()>>,
    /// The host daemon's epoch from its latest `hello` (kept while disconnected).
    pub epoch: Option<Epoch>,
}

/// Whether a machine is due for an automatic upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpgradeCheck {
    /// Auto-upgrade is off, nothing newer is bundled, or the machine isn't connected.
    Off,
    /// Due, but a host request is in flight.
    Busy,
    /// Due and idle.
    Idle,
}

pub(crate) fn host_key_info(key: &HostKey) -> HostKeyInfo {
    HostKeyInfo {
        algorithm: key.algorithm(),
        fingerprint: key.fingerprint(),
    }
}

impl MachineEntry {
    /// Only a connected daemon older than the bundled version qualifies (`update_available`
    /// is never set for a newer one), so this never downgrades.
    pub(crate) fn upgrade_check(&self, host_calls: bool) -> UpgradeCheck {
        let due = self.record.auto_upgrade
            && self.live.update_available
            && self.live.connectivity == Connectivity::Connected;
        if !due {
            UpgradeCheck::Off
        } else if host_calls {
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
            doctor: r.doctor.clone(),
            bootstrap: self.live.bootstrap.clone(),
        }
    }

    /// The machine as the agent daemon sees it.
    pub(crate) fn host_machine(&self) -> HostMachine {
        HostMachine {
            machine_id: self.record.id,
            name: self.record.name.clone(),
            availability: Availability {
                connectivity: self.live.connectivity.clone(),
                since: self.live.since,
            },
            epoch: self.epoch,
            grace_s: self.record.grace_s,
        }
    }
}
