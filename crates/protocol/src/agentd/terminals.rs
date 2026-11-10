//! The user's own terminals in a session's dock (`session.terminal_open`,
//! `session.terminal_close`): shells owned by the user ([`ShellOwner::User`]) on a workspace's
//! host, never visible to or reachable by the agent.
//!
//! [`ShellOwner::User`]: crate::session::ShellOwner::User

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{OpId, SessionId, WorkspaceId};
use crate::session::{DeviceRef, ShellSandbox};

/// Params of `session.terminal_open`: open a terminal of the user's own in a workspace. It is
/// listed in the session's summary (`owner: user`) and persists until it exits or is closed
/// (not reopened), the session is closed or deleted, or its workspace is removed.
/// `unsandboxed` requires `confirm: true` (`invalid_params` otherwise). The result is the new
/// shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TerminalOpenParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The workspace to open it in (its machine and directory).
    pub workspace_id: WorkspaceId,
    /// `workspace` (the workspace's sandbox, as the agent's shells) or `unsandboxed`.
    pub sandbox: ShellSandbox,
    /// The user confirmed an unsandboxed terminal.
    #[serde(default)]
    pub confirm: bool,
    /// The device that opened it (filled in by the controller), for the logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<DeviceRef>,
}

/// Params of `session.terminal_close`: end one of the user's terminals (its processes get
/// SIGHUP). The agent's shells can't be closed this way (`invalid_params`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TerminalCloseParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The terminal's shell name.
    pub name: String,
}
