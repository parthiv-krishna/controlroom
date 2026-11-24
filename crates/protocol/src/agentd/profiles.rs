//! Profiles on the agent daemon: the model policy the controller reports, and moving a session
//! to another profile.
//!
//! The controller owns profiles (UI API `profiles.*`) and reports their policies with
//! `profiles.report` after each `hello` and on every change, as it reports machines. The agent
//! daemon keeps the last report across restarts, so launches don't wait for the controller, and
//! enforces it: a session's agent sees only the accounts its profile admits (at launch, in
//! `session.set_option`, for subagents and in credential capture). A session whose profile is not
//! in the report admits no account (fail closed).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agentd::ModelDefaults;
use crate::ids::{OpId, ProfileId, SessionId};

/// Which model accounts a profile's agents may use.
///
/// An account is a pi provider id: a preset's first account is the preset's id
/// (`"anthropic"`), an added account has its own (`"anthropic-work"`), and gateways and
/// server-declared providers are their names. "A provider, any account" can't be expressed on
/// purpose: a new account never joins a profile silently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelPolicy {
    /// Every account, including ones added later. Never on a private profile.
    Any,
    /// Only these accounts.
    Only {
        /// The admitted accounts (pi provider ids).
        accounts: Vec<String>,
    },
    /// A policy this build doesn't know: it admits no account (fail closed).
    #[serde(other)]
    Unknown,
}

impl ModelPolicy {
    /// Whether the policy admits `account` (a pi provider id). [`ModelPolicy::Unknown`] admits
    /// none.
    pub fn admits(&self, account: &str) -> bool {
        match self {
            ModelPolicy::Any => true,
            ModelPolicy::Only { accounts } => accounts.iter().any(|a| a == account),
            ModelPolicy::Unknown => false,
        }
    }
}

/// One profile's policy, as the agent daemon enforces it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProfilePolicy {
    /// The profile.
    pub profile_id: ProfileId,
    /// The accounts its agents may use.
    pub models: ModelPolicy,
    /// The model and thinking level its new sessions start with, on top of the server-wide
    /// defaults (Settings › Models); all `null` keeps those. A default outside `models` is
    /// dropped at launch.
    #[serde(default)]
    pub model_defaults: ModelDefaults,
    /// A private profile (metadata-only push; its policy lists its accounts).
    #[serde(default)]
    pub private: bool,
}

/// Params of `profiles.report`: every profile's policy, replacing the previous report.
/// Changes apply to new sessions and restarts; `session.set_option` and subagent checks use the
/// current report at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProfilesReportParams {
    /// Every profile.
    pub profiles: Vec<ProfilePolicy>,
}

/// Params of `session.move`: move a session to another profile. The controller checks
/// membership first (every workspace's machine is in the target). The session takes the
/// target's policy; a live agent restarts and resumes, as for a sandbox-level change (`busy`
/// while a run is active or the agent is starting). Its schedules follow it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionMoveParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The target profile.
    pub profile_id: ProfileId,
}
