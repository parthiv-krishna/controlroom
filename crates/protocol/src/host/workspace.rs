//! Workspaces on a host (`workspace.open`, `workspace.close`) and their named shells
//! (`shell.open`, `shell.close`, `shell.list`).
//!
//! A workspace is one session's directory on this host with its own nono profile. Everything
//! the agent triggers there runs under that profile: the PTY shells, the jobs, and the fs worker
//! (`controlroomd workspace-exec`) that serves the file and search ops. Two sessions on one path
//! get two workspaces, each with its own profile and shells.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{RemotePath, SessionId, ShellId, WorkspaceId};
use crate::session::{DeviceRef, SandboxSpec, Shell, ShellOwner, ShellSandbox};

/// Params of `workspace.open`: resolve and check the directory and write the workspace's sandbox
/// profile. Idempotent by `workspace_id`; calling it again with a changed `sandbox` rewrites the
/// profiles (shells and jobs started later use them; running ones keep theirs until reopened).
/// A changed `level` also restarts the agent's shells under the new profile, in their last
/// directory; their next command reports the reset ("the sandbox level changed"). The user's
/// terminals and running jobs keep the sandbox they started with until they end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct WorkspaceOpenParams {
    /// The workspace (assigned by the agent daemon).
    pub workspace_id: WorkspaceId,
    /// The session it belongs to; shell ids are scoped by it.
    pub session_id: SessionId,
    /// The directory; `~` is expanded.
    pub path: RemotePath,
    /// The session's effective sandbox: `level` is the level the session runs at (its explicit
    /// level, else the defaults'), plus the standing grants, domains and mediated commands.
    pub sandbox: SandboxSpec,
}

/// An instruction file found for a workspace (`AGENTS.override.md`, `AGENTS.md` or `CLAUDE.md`,
/// the first that exists per directory, from `/` down to the workspace root, like pi).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ContextFile {
    /// The file, absolute.
    pub path: RemotePath,
    /// Its content (UTF-8, lossy).
    pub content: String,
}

/// A skill found under the workspace root (`.pi/skills`, `.agents/skills`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Skill {
    /// The skill's name.
    pub name: String,
    /// Its one-line description (from the skill file's front matter).
    pub description: String,
    /// Its `SKILL.md`, absolute.
    pub path: RemotePath,
}

/// Result of `workspace.open`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct WorkspaceOpenResult {
    /// The resolved directory (absolute, symlinks resolved). Relative paths of the workspace's
    /// tool calls resolve against it.
    pub root: RemotePath,
    /// The host user's home directory (what `~` means there).
    pub home: RemotePath,
    /// Instruction files, outermost first.
    pub context_files: Vec<ContextFile>,
    /// Workspace skills, by name.
    pub skills: Vec<Skill>,
}

/// Params of `workspace.close`: stop the workspace's shells and jobs and forget it.
/// Idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct WorkspaceCloseParams {
    /// The workspace.
    pub workspace_id: WorkspaceId,
}

/// A shell as the host knows it: the session's [`Shell`] plus its host id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HostShell {
    /// `<session id>/<name>`; terminals attach with it.
    pub shell_id: ShellId,
    /// The shell.
    #[serde(flatten)]
    pub shell: Shell,
}

/// Params of `shell.open`: start a login shell (a detached PTY supervisor under the workspace's
/// profile, with the shell integration that reports command boundaries and the cwd). Idempotent:
/// a live shell with this name is returned unchanged; a `gone` agent shell is reopened in its
/// last directory (or `cwd`) with `reset_pending` set.
///
/// The user's own terminals (`owner: user`) are never reopened: when their process ends they
/// are forgotten (`shell.changed` with `gone`), and a host restart that took their supervisor
/// drops them from `shell.list`. The agent's `tool.run` refuses them (`shell_gone`). An
/// `unsandboxed` one runs without nono; opening it is logged with `by`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ShellOpenParams {
    /// The workspace to run it in.
    pub workspace_id: WorkspaceId,
    /// The shell's name, unique within the session.
    pub name: String,
    /// The starting directory, absolute or relative to the workspace root; default the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<RemotePath>,
    /// Whose shell it is; default the agent's.
    #[serde(default)]
    pub owner: ShellOwner,
    /// How it is confined; only the user's terminals may be `unsandboxed`.
    #[serde(default)]
    pub sandbox: ShellSandbox,
    /// The device that opened a user's terminal, for the host's log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<DeviceRef>,
}

impl ShellOpenParams {
    /// Opens (or reopens) an agent shell.
    pub fn agent(workspace_id: WorkspaceId, name: String, cwd: Option<RemotePath>) -> Self {
        Self {
            workspace_id,
            name,
            cwd,
            owner: ShellOwner::Agent,
            sandbox: ShellSandbox::Workspace,
            by: None,
        }
    }
}

/// Params of `shell.close`: end the shell (SIGHUP to its group). Idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ShellCloseParams {
    /// The shell.
    pub shell_id: ShellId,
}

/// Params of `shell.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ShellListParams {
    /// Only this workspace's shells; default every shell on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

/// Result of `shell.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ShellListResult {
    /// The shells, by id.
    pub shells: Vec<HostShell>,
}

/// Params of the `shell.changed` notification: a shell's cwd, state or foreground process
/// changed, or it was opened or closed (`gone`). Sent to the controller connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ShellChangedParams {
    /// The shell, whole.
    pub shell: HostShell,
}
