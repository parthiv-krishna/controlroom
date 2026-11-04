//! The per-machine daemon's cwd-based `session.create` (deprecated, see the module docs).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agentd::{InitialPrompt, PromptResult};
use crate::ids::{OpId, RemotePath, SessionId};
use crate::session::{PeerRef, SandboxLevel, SandboxSpec, SessionSummary};

/// Params of `session.create`: create a session and start its agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionCreateParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The agent key in the machine's `agents.toml`.
    pub agent: String,
    /// The working directory; `~` is expanded by the daemon.
    pub cwd: RemotePath,
    /// A user-set name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The parent session on this machine, for subagents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<SessionId>,
    /// The spawning session on another machine, for cross-machine subagents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawned_by: Option<PeerRef>,
    /// An explicit level chosen by the user (new-session dialog or template). Honored in either
    /// direction, and kept for the session's life; absent: the default level applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_level: Option<SandboxLevel>,
    /// The sandbox defaults. Absent: the daemon's own (`strict`, no extra grants). In the UI
    /// API, the controller always fills it with the defaults of the machine and matching
    /// projects, replacing any value a client sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxSpec>,
    /// A first prompt, queued in the same transaction as the session (under this `op_id`), so
    /// a retried create never makes a session without its prompt or sends the prompt twice.
    /// It goes to the agent as soon as the agent is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<InitialPrompt>,
}

/// Result of `session.create`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SessionCreateResult {
    /// The new session (state `starting`).
    pub session: SessionSummary,
    /// What happened to the first prompt, if one was given: `queued` (it is sent once the
    /// agent is ready).
    pub prompt: Option<PromptResult>,
}
