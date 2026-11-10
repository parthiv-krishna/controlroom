//! What a session persists about its hosts: workspaces, shells and late tool calls.

use controlroom_protocol::host::{ContextFile, HostShell, Skill};
use controlroom_protocol::ids::{CallId, Epoch, MachineId, RemotePath, UnixMillis, WorkspaceId};
use controlroom_protocol::session::{
    Availability, Shell, ShellOwner, ShellSandbox, ShellState, Workspace,
};
use serde::{Deserialize, Serialize};

/// One workspace of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkspaceRecord {
    pub id: WorkspaceId,
    pub machine_id: MachineId,
    /// The directory as given.
    pub path: RemotePath,
    /// The directory the host resolved (`workspace.open`).
    pub root: Option<RemotePath>,
    /// The host user's home.
    pub home: Option<RemotePath>,
    pub added_at: UnixMillis,
    /// The workspace's default shell.
    pub shell: String,
    /// Instruction files from the latest `workspace.open`, outermost first.
    pub context_files: Vec<ContextFile>,
    /// Skills from the latest `workspace.open`.
    pub skills: Vec<Skill>,
}

impl WorkspaceRecord {
    /// The wire view; the first workspace of a session is its primary one.
    pub(crate) fn to_workspace(&self, primary: bool, availability: Availability) -> Workspace {
        Workspace {
            id: self.id,
            machine_id: self.machine_id,
            path: self.path.clone(),
            root: self.root.clone(),
            primary,
            added_at: self.added_at,
            availability,
        }
    }

    /// Whether `path` (absolute, or `~`-relative) lies inside the workspace's root.
    pub(crate) fn contains(&self, path: &str) -> bool {
        let Some(root) = &self.root else {
            return false;
        };
        let expanded = match (path.strip_prefix('~'), &self.home) {
            (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
                format!("{}{rest}", home.as_str().trim_end_matches('/'))
            }
            _ => path.to_owned(),
        };
        let root = root.as_str().trim_end_matches('/');
        expanded == root || expanded.starts_with(&format!("{root}/"))
    }
}

/// One named shell of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ShellRecord {
    pub shell: Shell,
    /// The host epoch the shell was last opened in; a restarted host gets `shell.open` again
    /// before the next command (it reopens the shell in its last directory).
    pub opened: Option<Epoch>,
}

impl ShellRecord {
    /// An agent shell not yet opened on its host.
    pub(crate) fn new(name: String, workspace_id: WorkspaceId, cwd: RemotePath) -> Self {
        Self::of(
            ShellOwner::Agent,
            ShellSandbox::Workspace,
            name,
            workspace_id,
            cwd,
        )
    }

    /// One of the user's terminals, not yet opened on its host.
    pub(crate) fn user(
        sandbox: ShellSandbox,
        name: String,
        workspace_id: WorkspaceId,
        cwd: RemotePath,
    ) -> Self {
        Self::of(ShellOwner::User, sandbox, name, workspace_id, cwd)
    }

    fn of(
        owner: ShellOwner,
        sandbox: ShellSandbox,
        name: String,
        workspace_id: WorkspaceId,
        cwd: RemotePath,
    ) -> Self {
        ShellRecord {
            shell: Shell {
                name,
                workspace_id,
                owner,
                sandbox,
                cwd,
                state: ShellState::Idle,
                reset_pending: false,
                foreground: None,
            },
            opened: None,
        }
    }

    /// Whether the agent may see and use it (an unknown owner counts as the user's).
    pub(crate) fn is_agents(&self) -> bool {
        self.shell.owner == ShellOwner::Agent
    }

    /// Takes the host's view of the shell (cwd, state, reset, foreground process). Its
    /// workspace, owner and sandbox stay the session's: a host never makes a user's terminal
    /// the agent's.
    pub(crate) fn update(&mut self, host: &HostShell) {
        let (workspace_id, owner, sandbox) = (
            self.shell.workspace_id,
            self.shell.owner,
            self.shell.sandbox,
        );
        self.shell = host.shell.clone();
        self.shell.workspace_id = workspace_id;
        self.shell.owner = owner;
        self.shell.sandbox = sandbox;
    }
}

/// A tool call that failed with `connection_lost`: its result is fetched with `tool.attach` when
/// the host is back, and injected into the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LateCall {
    pub call_id: CallId,
    pub machine_id: MachineId,
    /// What the call did, for the injected result (e.g. ``bash `make test` ``).
    pub what: String,
    /// When the call failed.
    pub failed_at: UnixMillis,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_inside_the_root() {
        let workspace = WorkspaceRecord {
            id: WorkspaceId::generate(),
            machine_id: MachineId::generate(),
            path: RemotePath::new("~/src/api"),
            root: Some(RemotePath::new("/home/alice/src/api")),
            home: Some(RemotePath::new("/home/alice")),
            added_at: UnixMillis(0),
            shell: "devbox".into(),
            context_files: Vec::new(),
            skills: Vec::new(),
        };
        assert!(workspace.contains("/home/alice/src/api"));
        assert!(workspace.contains("/home/alice/src/api/main.rs"));
        assert!(workspace.contains("~/src/api/x"));
        assert!(!workspace.contains("/home/alice/src/apix"));
        assert!(!workspace.contains("src/x"));
    }
}
