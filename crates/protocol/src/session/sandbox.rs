//! Sandbox levels, grants and the profile a session is started with.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{GrantId, UnixMillis};
use crate::session::{DeviceRef, PathAccess};

/// How strictly a session's agent is confined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
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

/// The effective sandbox profile of a session, computed by the controller from the machine and
/// project defaults and passed to the daemon when the session is created or (re)started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SandboxSpec {
    /// The level.
    pub level: SandboxLevel,
    /// Standing grants.
    pub grants: Vec<SandboxGrant>,
    /// Allowed network domains beyond the active providers' hosts.
    pub domains: Vec<String>,
    /// Mediated commands beyond the built-in defaults.
    pub mediated: Vec<MediatedCommand>,
}

/// A session's sandbox, as shown in the session header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SandboxInfo {
    /// The level the agent runs at.
    pub level: SandboxLevel,
    /// The session's grants (once, session and standing).
    pub grants: Vec<SandboxGrant>,
}
