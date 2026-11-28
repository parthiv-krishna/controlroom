//! Profiles (design `docs/design/profiles-tenants.md` §2–§4): the policies the controller
//! reports (`profiles.report`, persisted so that launches after a restart don't wait for it) and
//! the checks that keep a session's agent inside its profile:
//!
//! - **model accounts:** each launch gets the profile's model policy and defaults (the agent
//!   daemon's launch builds the session's agent dir from them: A1, A4, A6), and
//!   `session.set_option` refuses a model of another account (A3);
//! - **machines:** a workspace on a machine outside the profile (A9), and an agent's host call,
//!   shell or job there (A8), is `policy_denied`, checked against the machine report.
//!
//! A session's profile is set at creation; a child's is its parent's (A5). Refusals are
//! `policy_denied` naming the session's own profile.
//!
//! **Fail closed.** A profile without a reported policy admits no account. A session without a
//! profile exists only when it was created while no profile was reported (a debug client, or a
//! controller from before profiles): it is unrestricted as long as no profile is reported, and
//! admits no account and no machine once one is; creating one is then refused.

use std::collections::BTreeMap;

use controlroom_protocol::agentd::{
    ModelDefaults, ModelPolicy, ProfilePolicy, SessionCreateParams,
};
use controlroom_protocol::ids::{MachineId, ProfileId};
use controlroom_protocol::{ErrorData, Resource, RpcError};

use crate::core::Core;
use crate::errors::{invalid_params, not_found};
use crate::model::SessionData;

/// The reported policies, by profile.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Profiles(BTreeMap<ProfileId, ProfilePolicy>);

impl Profiles {
    pub(crate) fn new(policies: Vec<ProfilePolicy>) -> Self {
        Self(policies.into_iter().map(|p| (p.profile_id, p)).collect())
    }

    /// Every policy, in profile order.
    pub(crate) fn policies(&self) -> Vec<ProfilePolicy> {
        self.0.values().cloned().collect()
    }

    /// No profile was reported (a controller from before profiles, or none connected yet).
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The accounts sessions of `profile` may use (see the module docs).
    pub(crate) fn models(&self, profile: Option<ProfileId>) -> ModelPolicy {
        match profile {
            Some(id) => self
                .0
                .get(&id)
                .map_or_else(no_account, |p| p.models.clone()),
            None if self.is_empty() => ModelPolicy::Any,
            None => no_account(),
        }
    }

    /// The model defaults new agents of `profile` start with (on top of the server's).
    pub(crate) fn model_defaults(&self, profile: Option<ProfileId>) -> ModelDefaults {
        profile
            .and_then(|id| self.0.get(&id))
            .map(|p| p.model_defaults.clone())
            .unwrap_or_default()
    }
}

/// The policy of a profile that admits nothing.
fn no_account() -> ModelPolicy {
    ModelPolicy::Only {
        accounts: Vec::new(),
    }
}

/// A refusal by the session's profile (`profile`).
fn denied(profile: Option<ProfileId>, reason: String) -> RpcError {
    match profile {
        Some(profile_id) => RpcError::new(
            ErrorData::PolicyDenied {
                profile_id,
                reason: reason.clone(),
            },
            reason,
        ),
        None => invalid_params(format!("{reason} (the session has no profile)")),
    }
}

impl Core {
    /// `profiles.report`: replaces the policies (saved with the transaction). Launches and
    /// restarts from now on use them, and so do the checks.
    /// A profile whose model policy changed is recorded for `stale_credentials`.
    pub(crate) fn set_profiles(&mut self, report: Vec<ProfilePolicy>) {
        let profiles = Profiles::new(report);
        if profiles == self.profiles {
            return;
        }
        let changed: Vec<ProfileId> = profiles
            .0
            .values()
            .filter(|p| {
                self.profiles.0.get(&p.profile_id).map(|old| &old.models) != Some(&p.models)
            })
            .map(|p| p.profile_id)
            .collect();
        *self.profiles_mut() = profiles;
        if !changed.is_empty() {
            self.policies_changed(&changed);
        }
    }

    /// The profile of a session `params` creates: its parent's for a child, else the one asked
    /// for. Every workspace's machine must be in it.
    pub(crate) fn new_session_profile(
        &self,
        params: &SessionCreateParams,
    ) -> Result<Option<ProfileId>, RpcError> {
        let profile = match params.parent_id {
            Some(parent) => {
                let inherited = self.session(parent)?.data.profile_id;
                if params.profile_id.is_some() && params.profile_id != inherited {
                    return Err(denied(
                        inherited,
                        "a subagent runs in its parent's profile".into(),
                    ));
                }
                inherited
            }
            None => params.profile_id,
        };
        if profile.is_none() && !self.profiles.is_empty() {
            return Err(invalid_params("a session needs a profile"));
        }
        for workspace in &params.workspaces {
            self.require_machine(profile, &workspace.machine_id)?;
        }
        Ok(profile)
    }

    /// `policy_denied` unless `machine` is in `profile` (per the machine report).
    pub(crate) fn require_machine(
        &self,
        profile: Option<ProfileId>,
        machine: &MachineId,
    ) -> Result<(), RpcError> {
        let admitted = match profile {
            Some(id) => self
                .machines
                .list
                .get(machine)
                .is_some_and(|m| m.profile_ids.contains(&id)),
            None => self.profiles.is_empty(),
        };
        if admitted {
            return Ok(());
        }
        let name = self.machines.name(machine);
        Err(denied(
            profile,
            format!("{name} is not in this session's profile"),
        ))
    }

    /// The machine the agent names (`machine:path`): one of the session's workspaces'
    /// (`not_found` `workspace` otherwise) and in its profile (`policy_denied`).
    pub(crate) fn agent_machine(
        &self,
        data: &SessionData,
        name: &str,
    ) -> Result<MachineId, RpcError> {
        let machine = self
            .machines
            .by_name(name)
            .filter(|m| data.workspaces.iter().any(|w| w.machine_id == *m))
            .ok_or_else(|| not_found(Resource::Workspace, name))?;
        self.require_machine(data.profile_id, &machine)?;
        Ok(machine)
    }

    /// `policy_denied` unless the session's profile admits `account` (a pi provider id).
    pub(crate) fn require_account(
        &self,
        data: &SessionData,
        account: &str,
    ) -> Result<(), RpcError> {
        if self.profiles.models(data.profile_id).admits(account) {
            return Ok(());
        }
        Err(denied(
            data.profile_id,
            format!("{account} is not available in this session's profile"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(profile_id: ProfileId, models: ModelPolicy) -> ProfilePolicy {
        ProfilePolicy {
            profile_id,
            models,
            model_defaults: ModelDefaults {
                provider: Some("local".into()),
                model: None,
                thinking: None,
            },
            private: false,
        }
    }

    #[test]
    fn unreported_profiles_admit_nothing() {
        let (listed, any, unreported) = (
            ProfileId::generate(),
            ProfileId::generate(),
            ProfileId::generate(),
        );
        let only = ModelPolicy::Only {
            accounts: vec!["local".into()],
        };
        let profiles = Profiles::new(vec![
            policy(listed, only.clone()),
            policy(any, ModelPolicy::Any),
        ]);
        assert_eq!(profiles.models(Some(listed)), only);
        assert_eq!(profiles.models(Some(any)), ModelPolicy::Any);
        assert!(!profiles.models(Some(unreported)).admits("local"));
        assert_eq!(
            profiles.model_defaults(Some(listed)).provider.as_deref(),
            Some("local")
        );
        assert_eq!(
            profiles.model_defaults(Some(unreported)),
            ModelDefaults::default()
        );
        // A session without a profile: unrestricted only while no profile is reported.
        assert!(!profiles.models(None).admits("local"));
        assert_eq!(Profiles::default().models(None), ModelPolicy::Any);
    }
}
