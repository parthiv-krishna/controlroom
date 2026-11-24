//! Profiles: tenants of one owner (Settings, the rail and every scoped view).
//!
//! A profile owns machine entries ([`super::Machine::profile_ids`]), the model accounts its agents
//! may use ([`ModelPolicy`]) with its model defaults, projects and their tasks, sessions with
//! their schedules, sandbox defaults ([`super::DefaultsScope::Profile`]) and contact (which never
//! crosses profiles). Profiles are entirely the user's: names, colors and whether one is private
//! are chosen freely, and none is built in. A fresh install has no profile; creating the first
//! one is part of setup. Every write is checked by the servers; a refusal is `policy_denied`.
//!
//! A **private** profile pushes metadata only ([`super::PushContent::Private`]), never shares a
//! machine entry, and lists its accounts (its policy is never `any`).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agentd::{ModelDefaults, ModelPolicy};
use crate::ids::{GrantId, MachineId, ProfileId, ProjectId, SessionId, TaskId, UnixMillis};

/// A profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Profile {
    /// The profile.
    pub id: ProfileId,
    /// Display name, unique per owner.
    pub name: String,
    /// A CSS color, e.g. `"#a78bfa"`.
    pub color: String,
    /// Private: metadata-only push, no shared machine entries, an explicit account list.
    pub private: bool,
    /// The model accounts its agents may use.
    pub models: ModelPolicy,
    /// The model and thinking level its new sessions start with, on top of the server-wide
    /// defaults; all `null` keeps those. Always within `models`.
    #[serde(default)]
    pub model_defaults: ModelDefaults,
    /// Sort key, ascending (fractional indexing).
    pub position: f64,
    /// When it was created.
    pub created_at: UnixMillis,
}

/// Result of `profiles.list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct ProfilesListResult {
    /// Every profile, in `position` order (the state feed carries the same list).
    pub profiles: Vec<Profile>,
}

/// Params of `profiles.create`. The new profile goes last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProfilesCreateParams {
    /// Display name, unique per owner.
    pub name: String,
    /// A CSS color.
    pub color: String,
    /// Private; default `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
    /// The model accounts; default `any`, or no account for a private profile. `any` on a
    /// private profile is `invalid_params`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<ModelPolicy>,
}

/// Params of `profiles.update`. Absent fields are left unchanged. A private profile with models
/// `any`, or model defaults outside its accounts, are `invalid_params`; making a profile with a
/// shared machine entry private is `policy_denied`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct ProfilesUpdateParams {
    /// The profile.
    pub profile_id: ProfileId,
    /// New name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Private or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private: Option<bool>,
    /// New model accounts (applies to new sessions and restarts; option changes and subagents
    /// are checked against it at once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<ModelPolicy>,
    /// New model defaults; all `null` clears them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_defaults: Option<ModelDefaults>,
    /// New sort key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<f64>,
}

/// Params of `profiles.delete`. Only an empty profile can be deleted: one with machine entries,
/// projects, tasks or open sessions is `invalid_state`. Its sandbox defaults and model policy go
/// with it; deleting the last one returns to the fresh-install state (no profiles).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProfilesDeleteParams {
    /// The profile.
    pub profile_id: ProfileId,
    /// Delete its archived (closed) sessions too; required when it has any (`invalid_state`
    /// otherwise), after the UI confirmed with their count.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub delete_archived: bool,
}

/// Params of `profiles.move`: move machine entries, projects (with their tasks) and sessions to
/// one profile, atomically. Only the owner moves things; agents can't.
///
/// - **Machines** change membership: the target replaces their profiles, or joins them with
///   `share` (never with a private profile, and a shared entry never runs at level off).
/// - **Projects** move with their tasks. A task also in a project that stays behind blocks the
///   move (`policy_denied`).
/// - **Sessions** move when every workspace's machine is in the target afterwards
///   (`policy_denied` otherwise); a live agent restarts (`busy` while a run is active or the
///   agent is starting). Their schedules follow.
/// - **Task links and contact grants that would cross** profiles are removed.
///
/// With `dry_run`, nothing changes and the result previews the move (the dialog shows it before
/// the user confirms). Moving what is already in the target changes nothing, so a retry is safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProfilesMoveParams {
    /// The target profile.
    pub profile_id: ProfileId,
    /// Machine entries to move (or share).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub machine_ids: Vec<MachineId>,
    /// Projects to move, with their tasks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_ids: Vec<ProjectId>,
    /// Sessions to move (open or archived).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_ids: Vec<SessionId>,
    /// Add the target to the machines' profiles instead of replacing them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub share: bool,
    /// Only preview the move.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
}

/// A task's link to a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TaskLink {
    /// The task.
    pub task_id: TaskId,
    /// The linked session.
    pub session_id: SessionId,
}

/// Result of `profiles.move`: what moved (or, with `dry_run`, would move) and what breaks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProfilesMoveResult {
    /// The tasks that moved with their projects.
    pub task_ids: Vec<TaskId>,
    /// The sessions that moved; live agents among them restart.
    pub session_ids: Vec<SessionId>,
    /// Task links removed because they would cross profiles.
    pub unlinked: Vec<TaskLink>,
    /// Contact grants removed because they would cross profiles.
    pub revoked_grants: Vec<GrantId>,
    /// Open sessions that stay in their profile with a workspace on a machine that left it: that
    /// workspace becomes unavailable, and host calls there are refused.
    pub stranded: Vec<SessionId>,
}
