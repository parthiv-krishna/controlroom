//! Profiles in the hub: their feed events, the policy report to the agent daemon, and which
//! profile a session belongs to.

use std::collections::BTreeSet;

use controlroom_protocol::RpcError;
use controlroom_protocol::agentd::{ProfilePolicy, ProfilesReportParams};
use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::{MachineId, ProfileId};
use controlroom_protocol::session::SessionSummary;
use controlroom_protocol::ui::{Profile, StateEvent};

use super::{Hub, HubState};
use crate::errors::{not_found, store_error};

impl Hub {
    /// A stored profile, or `not_found`.
    pub(crate) fn profile(&self, id: ProfileId) -> Result<Profile, RpcError> {
        self.repo()
            .profiles()
            .get(id)
            .map_err(store_error)?
            .ok_or_else(|| not_found(Resource::Profile, id))
    }

    /// Every profile's policy, as `profiles.report` sends it.
    pub(crate) fn profile_report(&self) -> Result<ProfilesReportParams, RpcError> {
        let profiles = self.repo().profiles().list().map_err(store_error)?;
        Ok(ProfilesReportParams {
            profiles: profiles
                .into_iter()
                .map(|p| ProfilePolicy {
                    profile_id: p.id,
                    models: p.models,
                    model_defaults: p.model_defaults,
                    private: p.private,
                })
                .collect(),
        })
    }

    /// Queues a profile report for the agent daemon (after its `hello` and on every change).
    pub(crate) fn report_profiles(&self) {
        match self.profile_report() {
            Ok(report) => self.outbound.profiles(report),
            Err(e) => tracing::warn!(error = %e.message, "reading the profiles to report"),
        }
    }

    /// A session's profile for its push, and the profile's name when it is private. A session
    /// the controller can't place counts as private if any of its machines is in a private
    /// profile, and so does every session while the profiles can't be read (fail closed).
    pub(crate) fn push_profile(
        &self,
        session: &SessionSummary,
    ) -> (Option<ProfileId>, Option<String>) {
        let profiles = self.repo().profiles().list();
        let st = self.lock();
        let profile = st.session_profile(session);
        let profiles = match profiles {
            Ok(profiles) => profiles,
            Err(e) => {
                tracing::warn!(error = %e, "reading the profiles for a push");
                return (profile, Some(String::new()));
            }
        };
        let private = match profile {
            Some(id) => profiles.iter().find(|p| p.id == id && p.private),
            None => profiles.iter().find(|p| {
                p.private
                    && session
                        .workspaces
                        .iter()
                        .any(|w| st.machine_profiles(w.machine_id).contains(&p.id))
            }),
        };
        (profile, private.map(|p| p.name.clone()))
    }

    /// Emits a profile and reports the policies.
    pub(crate) fn emit_profile(&self, st: &mut HubState, profile: &Profile) {
        st.emit(StateEvent::ProfileUpserted {
            profile: profile.clone(),
        });
        self.report_profiles();
    }

    /// Emits a profile's removal and reports the policies.
    pub(crate) fn emit_profile_removed(&self, st: &mut HubState, profile_id: ProfileId) {
        st.emit(StateEvent::ProfileRemoved { profile_id });
        self.report_profiles();
    }
}

impl HubState {
    /// A machine's name for a message (its id once it is gone).
    pub(crate) fn machine_label(&self, id: MachineId) -> String {
        self.machines
            .get(&id)
            .map_or_else(|| id.to_string(), |e| e.record.name.clone())
    }

    /// A machine's profiles (none for an unknown machine).
    pub(crate) fn machine_profiles(&self, id: MachineId) -> &[ProfileId] {
        self.machines
            .get(&id)
            .map_or(&[], |e| e.record.profile_ids.as_slice())
    }

    /// The profile a session belongs to: the agent daemon's `profile_id`, else (a session from
    /// an agent daemon before profiles) the one profile all its workspaces' machines share.
    pub(crate) fn session_profile(&self, session: &SessionSummary) -> Option<ProfileId> {
        session.profile_id.or_else(|| {
            let mut common: Option<BTreeSet<ProfileId>> = None;
            for w in &session.workspaces {
                let here: BTreeSet<ProfileId> = self
                    .machine_profiles(w.machine_id)
                    .iter()
                    .copied()
                    .collect();
                common = Some(match common {
                    Some(c) => c.intersection(&here).copied().collect(),
                    None => here,
                });
            }
            let common = common?;
            if common.len() == 1 {
                common.into_iter().next()
            } else {
                None
            }
        })
    }
}
