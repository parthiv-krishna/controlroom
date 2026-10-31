//! Machine-level queries and settings: `agents.list`, `fs.list`, `doctor.report` and
//! `settings.*`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{OpId, RemotePath, UnixMillis};
use crate::session::DriverKind;

/// Whether an agent's command resolves through the login shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Found; usable for new sessions.
    Ok,
    /// The command (or something it needs) is missing.
    Missing,
    /// The check couldn't run (also the fallback for unknown values).
    #[serde(other)]
    Unknown,
}

/// A configured agent and its dependency check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentInfo {
    /// The key in `agents.toml`, e.g. `"pi"`.
    pub agent: String,
    /// The driver that runs it.
    pub driver: DriverKind,
    /// The configured command.
    pub command: String,
    /// The check result.
    pub status: AgentStatus,
    /// The resolved program path, when found.
    pub path: Option<RemotePath>,
    /// Explanation, e.g. why it is missing.
    pub detail: String,
}

/// Result of `agents.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentsListResult {
    /// Every configured agent.
    pub agents: Vec<AgentInfo>,
}

/// Params of `fs.list`, for the working-directory picker. Lists with the daemon user's access,
/// not a session sandbox's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FsListParams {
    /// The directory; `~` is expanded.
    pub path: RemotePath,
    /// Also list non-directories; default: directories only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_files: bool,
}

/// The kind of a directory entry (symlinks are not followed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FsEntryKind {
    /// A directory.
    Dir,
    /// A regular file.
    File,
    /// A symbolic link (listed in directories-only mode when it resolves to a directory).
    Symlink,
    /// A socket, FIFO or device.
    Other,
    /// A kind this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// One directory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FsEntry {
    /// The file name.
    pub name: String,
    /// Its kind.
    pub kind: FsEntryKind,
    /// Size in bytes, for files.
    pub size: Option<u64>,
    /// Last modification time, when known.
    pub mtime: Option<UnixMillis>,
}

/// Result of `fs.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FsListResult {
    /// The listed directory, with `~` expanded.
    pub path: RemotePath,
    /// Its entries, sorted by name.
    pub entries: Vec<FsEntry>,
}

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

/// How long closed sessions are kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RetentionPolicy {
    /// Purge closed sessions (transcripts included) this many days after closing; `null` keeps
    /// them forever (the default).
    pub delete_closed_after_days: Option<u32>,
}

/// Per-machine settings stored by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DaemonSettings {
    /// Stop an idle agent after this many hours (default 48), only when it is resumable, its
    /// queue is empty and it has no live work.
    pub idle_timeout_h: u32,
    /// Retention of closed sessions.
    pub retention: RetentionPolicy,
}

/// Params of `settings.set`. Absent fields are left unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SettingsSetParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// New idle timeout in hours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_h: Option<u32>,
    /// New retention policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<RetentionPolicy>,
}
