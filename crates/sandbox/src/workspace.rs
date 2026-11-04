//! The sandbox of a workspace on an execution host (spec §11, "Host, per workspace").
//!
//! Everything the agent triggers in a workspace runs under its profile: the PTY shells, the
//! jobs, and the fs worker that serves file and search ops. Unlike an agent session's sandbox
//! it keeps the host user's `HOME` (so `~` means the same in a shell as in a tool path, though
//! at level strict nothing there is readable outside the workspace root), has no agent dir,
//! no credential routes and no agent socket.
//!
//! ```text
//! <state>/host/workspaces/<id>/              the workspace's own dir (protected)
//! <state>/host/workspaces/<id>/profile.json  the generated profile
//! <state>/host/workspaces/<id>/tmp/          TMPDIR (read-write in the sandbox)
//! ```

use std::collections::BTreeMap;

use crate::names::{Name, SessionId};
use crate::paths::AbsPath;
use crate::pins::CommandPin;
use crate::policy::{EffectivePolicy, PathGrant};
use crate::spec::Approvals;

/// Everything needed to generate one workspace's nono profile.
#[derive(Debug, Clone)]
pub struct WorkspaceSandboxSpec {
    /// The workspace id (a ULID).
    pub id: SessionId,
    /// The workspace root (read-write; the working directory).
    pub root: AbsPath,
    /// The workspace's own directory in the host daemon's state (protected: holds the
    /// profile and the webhook secret).
    pub dir: AbsPath,
    /// The sandbox's `TMPDIR` (read-write), inside `dir`.
    pub tmp: AbsPath,
    /// The merged policy (level, standing grants, domains, command rules).
    pub policy: EffectivePolicy,
    /// Readable paths: system paths such as `/nix/store`, plus anything the host daemon
    /// shows the agent (e.g. command output logs).
    pub read_only: Vec<AbsPath>,
    /// Read-only home configuration added at [`crate::SandboxLevel::Relaxed`].
    pub relaxed_reads: Vec<PathGrant>,
    /// Existing, non-writable directories whose programs the sandbox may run besides `PATH`'s.
    pub exec_dirs: Vec<AbsPath>,
    /// SSH configuration `git` may read when it runs `ssh` (e.g. `~/.ssh`); nothing else
    /// in the sandbox can.
    pub ssh_config: Vec<AbsPath>,
    /// Paths no grant may cover (the host daemon's state and data dirs). `dir` is always
    /// protected.
    pub protected: Vec<AbsPath>,
    /// The approval webhook; required for approve decisions and capability elevation.
    pub approvals: Option<Approvals>,
    /// Pins for the commands named in `policy.commands`.
    pub pins: BTreeMap<Name, CommandPin>,
}

impl WorkspaceSandboxSpec {
    /// The relaxed-level reads if the level is relaxed, otherwise nothing.
    pub fn active_relaxed_reads(&self) -> &[PathGrant] {
        if self.policy.level == crate::SandboxLevel::Relaxed {
            &self.relaxed_reads
        } else {
            &[]
        }
    }
}
