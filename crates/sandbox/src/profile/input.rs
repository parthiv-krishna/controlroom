//! What generation and validation read: one view over the two kinds of sandbox (an agent
//! session on the server, a workspace on a host), so both profiles come from the same code.

use std::collections::BTreeMap;

use crate::names::{Name, SessionId};
use crate::paths::AbsPath;
use crate::pins::CommandPin;
use crate::policy::{EffectivePolicy, PathGrant};
use crate::spec::{Approvals, CredentialHelper, CredentialRoute, SessionSandboxSpec};
use crate::workspace::WorkspaceSandboxSpec;

/// The inputs of one profile.
pub(super) struct Input<'a> {
    /// `meta.name`.
    pub name: String,
    /// The working directory (read-write).
    pub cwd: &'a AbsPath,
    /// The merged policy.
    pub policy: &'a EffectivePolicy,
    /// Readable system paths.
    pub read_only: &'a [AbsPath],
    /// Relaxed-level reads, if the level is relaxed.
    pub relaxed_reads: &'a [PathGrant],
    /// Executable directories besides `PATH`'s.
    pub exec_dirs: &'a [AbsPath],
    /// Read-write directories besides the working directory.
    pub writable: Vec<AbsPath>,
    /// Variables set in the sandbox.
    pub set_vars: BTreeMap<&'static str, String>,
    /// Sockets the sandbox may connect to.
    pub unix_sockets: Vec<AbsPath>,
    /// Where the sandbox may bind sockets (its TMPDIR).
    pub socket_bind: AbsPath,
    /// Directories a mediated command reads and writes (besides grants).
    pub command_dirs: Vec<AbsPath>,
    /// What `ssh` run by `git` may read.
    pub ssh_reads: Vec<AbsPath>,
    /// Paths no grant may cover.
    pub protected: Vec<AbsPath>,
    /// Paths nono's built-in policy denies (none for agent sessions, which have no grants).
    pub nono_denied: &'a [AbsPath],
    /// Credential routes.
    pub routes: &'a [CredentialRoute],
    /// The credential helper and the session it serves (with routes).
    pub capture: Option<(&'a CredentialHelper, &'a SessionId)>,
    /// The approval webhook.
    pub approvals: Option<&'a Approvals>,
    /// Pins of mediated commands.
    pub pins: &'a BTreeMap<Name, CommandPin>,
}

impl<'a> Input<'a> {
    /// An agent session's sandbox (spec §11.1, §11.2).
    pub fn session(spec: &'a SessionSandboxSpec) -> Self {
        let layout = &spec.layout;
        let set_vars = BTreeMap::from([
            ("HOME", layout.home().to_string()),
            ("TMPDIR", layout.tmp().to_string()),
            ("PI_CODING_AGENT_DIR", layout.agent().to_string()),
        ]);
        let mut protected = spec.protected.clone();
        protected.push(layout.root().clone());
        Self {
            name: format!("cr-{}", layout.id()),
            cwd: &spec.cwd,
            policy: &spec.policy,
            read_only: &spec.read_only,
            relaxed_reads: spec.active_relaxed_reads(),
            exec_dirs: &spec.exec_dirs,
            writable: layout.sandbox_dirs().to_vec(),
            set_vars,
            unix_sockets: spec.daemon_socket.iter().cloned().collect(),
            socket_bind: layout.tmp(),
            command_dirs: vec![spec.cwd.clone(), layout.home(), layout.tmp()],
            ssh_reads: vec![layout.home(), layout.tmp()],
            protected,
            nono_denied: &[],
            routes: &spec.credential_routes,
            capture: Some((&spec.credential_helper, layout.id())),
            approvals: spec.approvals.as_ref(),
            pins: &spec.pins,
        }
    }

    /// A workspace's sandbox on a host (spec §11: host, per workspace).
    pub fn workspace(spec: &'a WorkspaceSandboxSpec) -> Self {
        let mut protected = spec.protected.clone();
        protected.push(spec.dir.clone());
        let mut ssh_reads = vec![spec.tmp.clone()];
        ssh_reads.extend(spec.ssh_config.iter().cloned());
        Self {
            name: format!("cr-ws-{}", spec.id),
            cwd: &spec.root,
            policy: &spec.policy,
            read_only: &spec.read_only,
            relaxed_reads: spec.active_relaxed_reads(),
            exec_dirs: &spec.exec_dirs,
            writable: vec![spec.tmp.clone()],
            set_vars: BTreeMap::from([("TMPDIR", spec.tmp.to_string())]),
            unix_sockets: Vec::new(),
            socket_bind: spec.tmp.clone(),
            command_dirs: vec![spec.root.clone(), spec.tmp.clone()],
            ssh_reads,
            protected,
            nono_denied: &spec.nono_denied,
            routes: &[],
            capture: None,
            approvals: spec.approvals.as_ref(),
            pins: &spec.pins,
        }
    }
}
