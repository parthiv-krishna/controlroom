//! Workspaces and shells: where a session's tools run.
//!
//! pi runs on the server; a session reaches files only through its **workspaces**, each a
//! `machine:path` on an execution host. A session has one or more, on any machines, and two
//! sessions may share a `machine:path` (each with its own shells; the UI warns). Each workspace
//! has named, stateful **shells** (PTYs on its host) that the agent's `bash` runs in and the user
//! can attach to as terminals.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{CallId, MachineId, RemotePath, UnixMillis, WorkspaceId};
use crate::ui::Connectivity;

/// A workspace to create: a directory on a machine. Params of `session.create` and
/// `session.workspace_add`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
pub struct WorkspaceSpec {
    /// The machine.
    pub machine_id: MachineId,
    /// The directory; `~` is expanded by the host.
    pub path: RemotePath,
}

/// Whether a workspace's machine can be reached, as the controller last reported it. While it
/// can't, the session keeps its state: tool calls on that machine wait (up to the grace period),
/// and the UI shows the workspace (not the session) as unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Availability {
    /// The controller's link to the machine's host daemon.
    pub connectivity: Connectivity,
    /// While connected: since when (a new value means a new connection). Otherwise: since when
    /// it has been unavailable (the last disconnect, kept across reconnect attempts).
    pub since: UnixMillis,
}

impl Availability {
    /// Whether calls go through right now.
    pub fn is_connected(&self) -> bool {
        self.connectivity == Connectivity::Connected
    }
}

/// One workspace of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Workspace {
    /// The workspace.
    pub id: WorkspaceId,
    /// Its machine.
    pub machine_id: MachineId,
    /// The directory as given (may start with `~`).
    pub path: RemotePath,
    /// The directory resolved by the host (`~` expanded, symlinks resolved); `null` until the
    /// host has opened the workspace.
    pub root: Option<RemotePath>,
    /// The session's primary workspace (the first one): relative paths and new shells default
    /// to it.
    pub primary: bool,
    /// When it was added.
    pub added_at: UnixMillis,
    /// Whether its machine can be reached.
    pub availability: Availability,
}

/// What a shell is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ShellState {
    /// At the prompt.
    Idle,
    /// Running an agent command.
    Running {
        /// The tool call.
        call_id: CallId,
    },
    /// The agent's command waits for terminal input (a password, a confirmation); the user
    /// types it in the terminal.
    WaitingInput {
        /// The tool call.
        call_id: CallId,
    },
    /// The shell's process is gone (its host rebooted, or it was closed); the next command
    /// reopens it in its last directory.
    Gone,
    /// A state this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// Whose shell it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ShellOwner {
    /// The agent's: its `bash` runs there; the user watches and types along.
    #[default]
    Agent,
    /// The user's own terminal: private, never listed, readable or reachable by the agent.
    User,
    /// An owner this build doesn't know; treated as the user's (hidden from the agent).
    #[serde(other)]
    Unknown,
}

/// How a shell is confined.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ShellSandbox {
    /// The workspace's sandbox profile (every agent shell).
    #[default]
    Workspace,
    /// No sandbox: a login shell as the host's SSH user (only the user's own terminals, each
    /// confirmed when opened).
    Unsandboxed,
    /// A mode this build doesn't know; shown as unsandboxed.
    #[serde(other)]
    Unknown,
}

/// A named shell of a session: a PTY on a workspace's host whose state (cwd, environment,
/// functions, venv) persists between the agent's commands and the user's typing. The UI shows
/// one terminal tab per shell.
///
/// The user's own terminals are shells too ([`ShellOwner::User`]): they are in the session's
/// summary for the UI, and never in what the agent sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Shell {
    /// Unique within the session, e.g. `"devbox"` or `"devbox-2"`; the user's terminals are
    /// named `you@<workspace's default shell>[-<n>]` (agent shell names have no `@`).
    pub name: String,
    /// The workspace it runs in (under that workspace's sandbox).
    pub workspace_id: WorkspaceId,
    /// Whose shell it is.
    #[serde(default)]
    pub owner: ShellOwner,
    /// How it is confined.
    #[serde(default)]
    pub sandbox: ShellSandbox,
    /// Its working directory, from the shell's OSC 7 reports.
    pub cwd: RemotePath,
    /// What it is doing.
    pub state: ShellState,
    /// The shell was reopened (after `exit`, or a host restart without its supervisor) and lost
    /// its variables; the agent's next result in it says so.
    pub reset_pending: bool,
    /// The PTY's foreground process name (e.g. `"vim"`), for the terminal's key pass-through;
    /// `null` at the prompt or when unknown.
    pub foreground: Option<String>,
}
