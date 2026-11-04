//! Where the host role keeps its files, under the daemon state dir.
//!
//! ```text
//! <state>/run/host.sock, run/host.lock          the host daemon socket and instance lock
//! <state>/run/<id>.sock                         shell and job supervisor sockets
//! <state>/host.log                              stderr of a detached host daemon
//! <state>/host/host.sqlite                      workspaces, shells, the journal, jobs, events
//! <state>/host/webhook.port                     the approval webhook's loopback port
//! <state>/host/workspaces/<id>/                 one workspace (protected from its sandbox):
//!     profile.json, hook.secret                 its nono profile and webhook secret
//!     tmp/                                      the sandbox's TMPDIR (read-write)
//!     view/                                     read-only for the sandbox:
//!         bashrc                                the shell integration
//!         cmds/<marker>.sh                      agent commands, sourced by the shell
//!         calls/<marker>.log                    each command's full output (`log_path`)
//! <state>/host/shells/<supervisor id>/          shell supervisors (PTY transcripts)
//! <state>/host/jobs/<job id>/                   job supervisors
//! ```
//!
//! Job logs, which the agent reads, live in the workspace: `<root>/.controlroom/jobs/<id>.log`.

use controlroom_sandbox::AbsPath;

use crate::dirs::Dirs;

/// The host role's paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPaths {
    dirs: Dirs,
}

impl HostPaths {
    /// The host layout inside `dirs`' state dir.
    pub fn new(dirs: Dirs) -> Self {
        Self { dirs }
    }

    /// The daemon's directories.
    pub fn dirs(&self) -> &Dirs {
        &self.dirs
    }

    /// `<state>/host`.
    pub fn root(&self) -> AbsPath {
        self.dirs.state().join("host")
    }

    /// `<state>/run/host.sock`.
    pub fn socket(&self) -> AbsPath {
        self.dirs.run().join("host.sock")
    }

    /// `<state>/run/host.lock`, held by the running host daemon.
    pub fn lock(&self) -> AbsPath {
        self.dirs.run().join("host.lock")
    }

    /// `<state>/host.log`: stderr of a detached host daemon.
    pub fn log(&self) -> AbsPath {
        self.dirs.state().join("host.log")
    }

    /// `<state>/host/host.sqlite`.
    pub fn database(&self) -> AbsPath {
        self.root().join("host.sqlite")
    }

    /// `<state>/host/webhook.port`.
    pub fn webhook_port(&self) -> AbsPath {
        self.root().join("webhook.port")
    }

    /// One workspace's files.
    pub fn workspace(&self, id: &str) -> WorkspacePaths {
        WorkspacePaths {
            dir: self.root().join("workspaces").join(id),
        }
    }

    /// The directory of a shell supervisor.
    pub fn shell_dir(&self, supervisor: &str) -> AbsPath {
        self.root().join("shells").join(supervisor)
    }

    /// The directory of a job supervisor.
    pub fn job_dir(&self, job: &str) -> AbsPath {
        self.root().join("jobs").join(job)
    }

    /// A supervisor socket.
    pub fn supervisor_socket(&self, id: &str) -> AbsPath {
        self.dirs.supervisor_socket(id)
    }
}

/// The files of one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePaths {
    dir: AbsPath,
}

impl WorkspacePaths {
    /// `workspaces/<id>`.
    pub fn dir(&self) -> &AbsPath {
        &self.dir
    }

    /// The generated nono profile.
    pub fn profile(&self) -> AbsPath {
        self.dir.join("profile.json")
    }

    /// The approval webhook secret.
    pub fn hook_secret(&self) -> AbsPath {
        self.dir.join("hook.secret")
    }

    /// The sandbox's `TMPDIR`.
    pub fn tmp(&self) -> AbsPath {
        self.dir.join("tmp")
    }

    /// What the sandbox may read but not change.
    pub fn view(&self) -> AbsPath {
        self.dir.join("view")
    }

    /// The shell integration (an rcfile).
    pub fn bashrc(&self) -> AbsPath {
        self.view().join("bashrc")
    }

    /// An agent command's file.
    pub fn command(&self, marker: &str) -> AbsPath {
        self.view().join("cmds").join(&format!("{marker}.sh"))
    }

    /// An agent command's full output.
    pub fn call_log(&self, marker: &str) -> AbsPath {
        self.view().join("calls").join(&format!("{marker}.log"))
    }

    /// The fs worker's stderr.
    pub fn worker_log(&self) -> AbsPath {
        self.dir.join("worker.log")
    }
}
