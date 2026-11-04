//! Remote tools (capability `remote_tools`): pi runs on the server with `--no-builtin-tools`,
//! and the extension registers `read write edit bash grep find ls` (same names and argument
//! shapes as pi's, paths as `machine:path`) plus `shell_new` and `shell_list`, all served by
//! `host.call` and `shell.*`.
//!
//! The agent names machines by their display name and shells by their name. The agent daemon
//! checks that `machine` is one of the session's workspaces (an agent can't name any other
//! machine: `not_found` `workspace`), routes the call to that machine's host, and keeps it open
//! while the host is unreachable, up to the machine's grace period (default 10 min); after that
//! the call fails with `connection_lost` and its late result is injected when the host returns.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::host::{ContextFile, HostOp, HostResult, Skill};
use crate::ids::{CallId, RemotePath};
use crate::session::{Availability, Shell};

/// One workspace as the agent sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct WorkspaceInfo {
    /// The machine's display name, as used in `machine:path`.
    pub machine: String,
    /// The directory as given.
    pub path: RemotePath,
    /// The resolved directory; `null` until the host has opened the workspace.
    pub root: Option<RemotePath>,
    /// The host user's home (what `~` means there); `null` until opened.
    pub home: Option<RemotePath>,
    /// The primary workspace: bare and relative paths default to it.
    pub primary: bool,
    /// The default shell's name for commands in this workspace.
    pub shell: String,
    /// Whether its machine can be reached.
    pub availability: Availability,
    /// Instruction files from `/` down to the root, outermost first (shown in the system prompt
    /// as `machine:path`; files shared by several workspaces appear once).
    pub context_files: Vec<ContextFile>,
    /// Skills under the root.
    pub skills: Vec<Skill>,
}

/// A shell as the agent sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentShell {
    /// Its machine's display name.
    pub machine: String,
    /// The shell.
    #[serde(flatten)]
    pub shell: Shell,
}

/// The result of `session.info` and the params of the `session.info` notification: where the
/// session's tools run. Sent after `hello` and whenever it changes (a workspace added, opened or
/// unavailable, a shell opened or moved, an instruction file edited), so the extension can
/// rebuild its system-prompt sections before the next run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionInfo {
    /// The workspaces, the primary one first.
    pub workspaces: Vec<WorkspaceInfo>,
    /// The shells, in creation order.
    pub shells: Vec<AgentShell>,
}

/// Params of `host.call`: one host operation of a tool. Idempotent by `call_id` end to end.
/// `job_*` ops are refused (`invalid_params`): jobs go through `job.*`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HostCallParams {
    /// pi's tool call id, or `<id>/<n>` for the steps of one tool.
    pub call_id: CallId,
    /// The machine's display name; it must be one of the session's workspaces.
    pub machine: String,
    /// What to do. Relative paths resolve against the machine's (primary) workspace root;
    /// `bash` names the shell.
    pub op: HostOp,
}

/// Result of `host.call`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HostCallResult {
    /// The host's result.
    pub result: HostResult,
    /// How many times the link to the host dropped while the call was open; the result still
    /// comes from the host's journal (the call ran once). Worth telling the model.
    pub link_drops: u32,
}

/// Params of `host.cancel`: the tool's abort signal. A running `bash` is interrupted (its result
/// follows with `cancelled`); otherwise nothing happens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HostCancelParams {
    /// The call.
    pub call_id: CallId,
}

/// Params of `shell.open` (`shell_new`): open a fresh login shell. Idempotent by name: an open
/// shell with this name is returned unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ShellNewParams {
    /// Idempotency key: the pi tool call id.
    pub call_id: CallId,
    /// The shell's name, unique within the session.
    pub name: String,
    /// The machine's display name; it must be one of the session's workspaces.
    pub machine: String,
    /// The starting directory on that machine; default its (primary) workspace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<RemotePath>,
}

/// Params of `shell.close`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ShellEndParams {
    /// Idempotency key: the pi tool call id.
    pub call_id: CallId,
    /// The shell.
    pub name: String,
}

/// Result of `shell.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentShells {
    /// The shells, in creation order.
    pub shells: Vec<AgentShell>,
}
