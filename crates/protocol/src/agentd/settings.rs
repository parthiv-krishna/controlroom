//! The agent daemon's agents (`agents.list`) and settings (`settings.*`).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{OpId, RemotePath};
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
