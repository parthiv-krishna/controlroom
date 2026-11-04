//! `doctor.report`: the environment checks of a host daemon or of the agent daemon.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agentd::AgentInfo;
use crate::ids::{RemotePath, UnixMillis};

/// The outcome of one environment check. Checks never fail the call: anything undeterminable is
/// `unknown`, with the reason in the detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Fine.
    Ok,
    /// Works, with caveats.
    Warn,
    /// Will cause problems.
    Fail,
    /// Couldn't be determined (also the fallback for unknown values).
    #[serde(other)]
    Unknown,
}

/// One environment check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DoctorCheck {
    /// The outcome.
    pub status: CheckStatus,
    /// The observed value, e.g. `"yes"` or a version; `null` when unknown.
    pub value: Option<String>,
    /// Human-readable explanation.
    pub detail: String,
}

/// How the daemon starts after a reboot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum BootStartMethod {
    /// Linger plus a systemd user unit.
    SystemdUser,
    /// A `crontab` `@reboot` entry.
    Cron,
    /// Not at boot; on the next connection.
    Lazy,
    /// A method this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// The `controlroomd doctor` report, returned by `doctor.report`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DoctorReport {
    /// The daemon version that ran the checks.
    pub daemon_version: String,
    /// The Unix user the daemon runs as, if known.
    pub user: Option<String>,
    /// Whether processes outlive logout (linger, `KillUserProcesses`, cgroups).
    pub persistence: DoctorCheck,
    /// The boot start method in use.
    pub boot_start: BootStartMethod,
    /// Details of the boot start choice.
    pub boot_start_check: DoctorCheck,
    /// Whether nono is available, with its version as the value.
    pub nono: DoctorCheck,
    /// The kernel's Landlock ABI version, when available.
    pub landlock_abi: Option<u32>,
    /// The `agents.toml` that was read; `null` when absent and defaults apply.
    pub agents_config: Option<RemotePath>,
    /// Set when `agents.toml` exists but could not be used.
    pub agents_config_error: Option<String>,
    /// Per-agent dependency checks.
    pub agents: Vec<AgentInfo>,
    /// When the checks ran.
    pub checked_at: UnixMillis,
}
