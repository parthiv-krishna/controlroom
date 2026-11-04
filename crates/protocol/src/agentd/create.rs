//! Creating sessions on workspaces, and adding or removing a session's workspaces.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agentd::{InitialPrompt, PromptResult};
use crate::ids::{OpId, SessionId, WorkspaceId};
use crate::session::SessionSummary;
use crate::session::{SandboxLevel, SandboxSpec, WorkspaceSpec};

/// Params of `session.create`: create a session on its workspaces and start its agent (pi, on
/// the server). Every workspace's machine must be connected (`machine_unavailable` otherwise);
/// the agent daemon opens the workspaces on their hosts before the agent's first run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionCreateParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The workspaces, at least one; the first is the primary one. Duplicates are merged.
    pub workspaces: Vec<WorkspaceSpec>,
    /// A user-set name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The parent session, for subagents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<SessionId>,
    /// An explicit level chosen by the user (new-session dialog or template). Honored in either
    /// direction, and kept for the session's life; absent: the default level applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_level: Option<SandboxLevel>,
    /// The sandbox defaults. Absent: `strict`, no extra grants. In the UI API the controller
    /// always fills it with the defaults of the workspaces' machines and the session's
    /// projects, replacing any value a client sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxSpec>,
    /// A first prompt, queued in the same transaction as the session (under this `op_id`), and
    /// sent once the agent is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<InitialPrompt>,
}

/// Result of `session.create`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SessionCreateResult {
    /// The new session (state `starting`).
    pub session: SessionSummary,
    /// What happened to the first prompt, if one was given: `queued`.
    pub prompt: Option<PromptResult>,
}

/// Params of `session.workspace_add`: give a session another workspace. The agent learns of it
/// through its `session.info` notification and a notice in its next turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct WorkspaceAddParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The new workspace; its machine must be connected (`machine_unavailable`).
    pub workspace: WorkspaceSpec,
}

/// Params of `session.workspace_remove`: drop a workspace (its shells and jobs stop). The
/// primary workspace can't be removed (`invalid_params`); a workspace with a running call or
/// job is `busy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct WorkspaceRemoveParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The workspace.
    pub workspace_id: WorkspaceId,
}
