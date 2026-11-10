//! Sandbox levels, grants and the profile a session is started with.
//!
//! **Level resolution.** The controller computes a session's *defaults* ([`SandboxSpec`]) from
//! its machine and every matching project: the strictest level, and the union of grants,
//! domains and mediated commands. An explicit level chosen for the session (in the new-session
//! dialog or a template) is an affirmative user choice and wins in either direction; without
//! one, the default level applies. Session grants add on top.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{GrantId, UnixMillis};
use crate::session::{DeviceRef, PathAccess};

/// How strictly a session's agent is confined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SandboxLevel {
    /// The working directory and the session's own directories only (the default).
    Strict,
    /// Adds read-only home configuration such as git's.
    Relaxed,
    /// No sandbox; shown with a warning badge.
    Off,
    /// A level this build doesn't know. Treat it as the strictest.
    #[serde(other)]
    Unknown,
}

impl SandboxLevel {
    /// Whether `self` confines less than `other`. Unknown levels count as the strictest.
    pub fn is_looser_than(self, other: SandboxLevel) -> bool {
        self.looseness() > other.looseness()
    }

    fn looseness(self) -> u8 {
        match self {
            SandboxLevel::Strict | SandboxLevel::Unknown => 0,
            SandboxLevel::Relaxed => 1,
            SandboxLevel::Off => 2,
        }
    }
}

/// What a sandbox grant allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    /// A path, with [`SandboxGrant::access`].
    Path,
    /// A network domain.
    Domain,
    /// A mediated command.
    Command,
    /// A kind this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// How long a sandbox grant lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum GrantScope {
    /// This one access.
    Once,
    /// The rest of this sandbox session (runtime grants; lost on restart).
    Session,
    /// Standing: part of the profile; applied on the next (re)start.
    Standing,
    /// A scope this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// One sandbox grant, of a session or of a machine or project default profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SandboxGrant {
    /// The grant.
    pub id: GrantId,
    /// What it allows.
    pub kind: GrantKind,
    /// The path, domain or command.
    pub value: String,
    /// The access, for path grants.
    pub access: Option<PathAccess>,
    /// How long it lasts.
    pub scope: GrantScope,
    /// When it was granted.
    pub granted_at: UnixMillis,
    /// The granting device, if granted by the user.
    pub by: Option<DeviceRef>,
}

/// What a mediated command's policy does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MediationAction {
    /// Ask the user each time (a `sandbox` request).
    Approve,
    /// Block direct calls.
    Deny,
    /// An action this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// A command the sandbox intercepts, e.g. `git push` (approve) or `ssh` (deny).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MediatedCommand {
    /// The command and optional leading arguments, e.g. `"git push"`.
    pub command: String,
    /// What happens when it runs.
    pub action: MediationAction,
}

/// A session's sandbox defaults, computed by the controller from the machine and project
/// defaults and passed to the daemon when the session is created or (re)started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SandboxSpec {
    /// The default level: the strictest of the matching scopes. Used unless the session has an
    /// explicit level.
    pub level: SandboxLevel,
    /// Standing grants.
    pub grants: Vec<SandboxGrant>,
    /// Allowed network domains beyond the active providers' hosts.
    pub domains: Vec<String>,
    /// Mediated commands beyond the built-in defaults.
    pub mediated: Vec<MediatedCommand>,
    /// Paths denied without asking ("never"): absolute or `~/…` (the host expands `~`). A
    /// file access request for one of them, or for anything under one, is denied at once and
    /// never shown. A standing grant covering the path still wins.
    #[serde(default)]
    pub never: Vec<String>,
}

/// A session's sandbox, as shown in the session header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SandboxInfo {
    /// The level the agent runs at: the explicit level if one was chosen, else the default.
    pub level: SandboxLevel,
    /// The level of the session's defaults (machine and projects).
    pub default_level: SandboxLevel,
    /// `level` was chosen explicitly for this session (new-session dialog or template). It
    /// stays across restarts; otherwise a restart picks up changed defaults.
    pub explicit: bool,
    /// `level` confines less than `default_level`: show the "looser than defaults" badge (`off`
    /// always shows the warning badge).
    pub looser_than_default: bool,
    /// The session's grants (once, session and standing).
    pub grants: Vec<SandboxGrant>,
}
