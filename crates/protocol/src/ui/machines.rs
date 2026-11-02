//! Machine records, connectivity, and the add / host-key / upgrade flows.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::daemon::{DaemonSettings, DoctorReport};
use crate::ids::{DaemonId, KeyId, MachineId, UnixMillis};
use crate::version::Capability;

/// Why reconnecting stopped until the user acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ActionRequiredReason {
    /// The host presented a different key than the pinned one (a hard error). Verify it and
    /// re-pin with `machines.confirm_host_key`.
    HostKeyChanged,
    /// The host rejected the key. Install it, then `machines.reconnect`.
    AuthFailed,
    /// No host key is pinned yet. Confirm it with `machines.confirm_host_key`.
    HostKeyUnconfirmed,
    /// Another controller is connected to the daemon (a daemon serves one controller at a
    /// time). Stop the other controller, then `machines.reconnect`.
    ControllerConflict,
    /// A reason this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// A host key presented by a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HostKeyInfo {
    /// The key algorithm, e.g. `"ssh-ed25519"`.
    pub algorithm: String,
    /// The fingerprint, e.g. `"SHA256:..."`.
    pub fingerprint: String,
}

/// The controller's link to a machine's daemon, shown separately from session state. Silence is
/// never failure: sessions of an unreachable machine keep their last known state, marked stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Connectivity {
    /// Connected and handshaken.
    Connected,
    /// Connecting, reconnecting, bootstrapping or upgrading.
    Connecting,
    /// Can't reach the machine; retrying with backoff.
    Unreachable,
    /// The daemon's protocol major differs; offer an upgrade.
    Incompatible,
    /// Reconnecting stopped until the user acts.
    ActionRequired {
        /// Why.
        reason: ActionRequiredReason,
        /// The key the host presented, for `host_key_changed` and `host_key_unconfirmed`.
        presented_host_key: Option<HostKeyInfo>,
    },
    /// A state this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// A step of the add-machine and upgrade flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapStep {
    /// Probing the platform, home directory and installed daemon version.
    Probe,
    /// Uploading, verifying (by running it) and installing the daemon binary.
    Install,
    /// Running `controlroomd doctor`.
    Doctor,
    /// Starting the daemon and handshaking.
    Start,
    /// A step this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// Live progress of a bootstrap or upgrade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct BootstrapProgress {
    /// The current step.
    pub step: BootstrapStep,
    /// The latest log line.
    pub message: String,
}

/// A machine record plus its live status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Machine {
    /// The record.
    pub id: MachineId,
    /// Display name; defaults to the host.
    pub name: String,
    /// Host name or address.
    pub host: String,
    /// SSH port.
    pub port: u16,
    /// SSH user; agents run as this user.
    pub user: String,
    /// The additional key used for this machine; `null` means the controlroom key.
    pub key_id: Option<KeyId>,
    /// The pinned host key; `null` until confirmed.
    pub host_key: Option<HostKeyInfo>,
    /// The daemon's id, once known. Daemon-side references ([`crate::session::PeerRef`]) name
    /// machines by it.
    pub daemon_id: Option<DaemonId>,
    /// The link to the daemon.
    pub connectivity: Connectivity,
    /// The installed daemon is older than the server's (and compatible); offer an upgrade.
    pub update_available: bool,
    /// Upgrade a compatible, older daemon automatically while the machine is idle.
    pub auto_upgrade: bool,
    /// When the daemon was last reachable.
    pub last_seen: Option<UnixMillis>,
    /// The installed daemon version, once known.
    pub version: Option<String>,
    /// The machine's architecture (`x86_64` or `aarch64`), once probed.
    pub arch: Option<String>,
    /// The last connection or bootstrap error.
    pub last_error: Option<String>,
    /// The daemon's optional features, from its `hello`.
    pub capabilities: Vec<Capability>,
    /// The daemon's settings, once connected.
    pub settings: Option<DaemonSettings>,
    /// The latest doctor report, once bootstrapped.
    pub doctor: Option<DoctorReport>,
    /// Progress of a running bootstrap or upgrade.
    pub bootstrap: Option<BootstrapProgress>,
}

/// The connection details for adding a machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NewMachine {
    /// Host name or address.
    pub host: String,
    /// SSH port.
    pub port: u16,
    /// SSH user.
    pub user: String,
    /// Display name; defaults to the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// An additional key to use instead of the controlroom key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<KeyId>,
}

/// Result of `machines.add`, step one: the host key to confirm. Nothing is saved yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MachinesAddResult {
    /// The key the host presented.
    pub host_key: HostKeyInfo,
}

/// Params of `machines.add_confirm`, step two: the same details plus the fingerprint the user
/// confirmed. Call it again after installing the key to retry a `permission_denied`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MachinesAddConfirmParams {
    /// The fingerprint from `machines.add`; a different key now is `host_key_mismatch`.
    pub host_key_fingerprint: String,
    /// The connection details.
    #[serde(flatten)]
    pub machine: NewMachine,
}

/// Result of `machines.add_confirm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum MachinesAddConfirmResult {
    /// Saved with the host key pinned. Bootstrap continues in the background; progress and
    /// doctor results arrive through the state feed.
    Added {
        /// The new record.
        machine: Box<Machine>,
    },
    /// The host rejected the key. Show the install command; retry once it is installed.
    PermissionDenied {
        /// A command that appends the public key to `~/.ssh/authorized_keys`.
        install_command: String,
        /// The public key that was offered.
        public_key: String,
    },
    /// An outcome this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// Params of machine actions that only name the machine: `machines.remove`,
/// `machines.reconnect`, `machines.upgrade`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MachineRef {
    /// The machine.
    pub machine_id: MachineId,
}

/// Params of `machines.confirm_host_key`: pin a machine's (new) host key after the user
/// verified it, then reconnect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MachinesConfirmHostKeyParams {
    /// The machine.
    pub machine_id: MachineId,
    /// The confirmed fingerprint; if the host now presents another key, `host_key_mismatch`.
    pub host_key_fingerprint: String,
}

/// Which key a machine uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MachineKeyChoice {
    /// An additional key, or `null` for the controlroom key.
    pub key_id: Option<KeyId>,
}

/// Params of `machines.set_settings`: the controller's machine settings. Absent fields are left
/// unchanged. Daemon settings go through the `settings.set` pass-through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MachinesSetSettingsParams {
    /// The machine.
    pub machine_id: MachineId,
    /// New display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New key selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<MachineKeyChoice>,
    /// Turn automatic daemon upgrades on or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_upgrade: Option<bool>,
}
