//! Profiles (`profiles.list`, `create`, `update`, `delete`): tenants of the owner, entirely
//! user-configured. Every change is emitted to the state feed and reported to the agent daemon
//! (`profiles.report`).
//!
//! - A private profile lists its accounts (models `any` is `invalid_params`, C9) and never
//!   shares a machine entry (making one with a shared entry private is `policy_denied`, C7).
//! - Model defaults must be within the profile's accounts (C9).
//! - Only an empty profile is deleted (C10): one with machine entries, projects, tasks or open
//!   sessions is `invalid_state`, and its archived sessions go with it once the request says so
//!   (`delete_archived`). Deleting the last one returns to the fresh-install state.

use std::sync::Arc;

use controlroom_protocol::agentd::methods::SessionDelete;
use controlroom_protocol::agentd::{ModelDefaults, ModelPolicy, SessionOpParams};
use controlroom_protocol::ids::{OpId, SessionId};
use controlroom_protocol::session::SessionState;
use controlroom_protocol::ui::{
    Profile, ProfilesCreateParams, ProfilesDeleteParams, ProfilesListResult, ProfilesUpdateParams,
};
use controlroom_protocol::{Ack, RpcError};

use super::{Conn, passthrough};
use crate::errors::{invalid_params, invalid_state, policy_denied, store_error};
use crate::store::{NewProfile, ProfileChanges, ProfileContents};

/// Longest profile name.
const MAX_NAME_CHARS: usize = 64;

fn name(name: &str) -> Result<String, RpcError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(invalid_params(format!(
            "a profile name has 1 to {MAX_NAME_CHARS} characters"
        )));
    }
    Ok(name.to_owned())
}

fn color(color: &str) -> Result<String, RpcError> {
    let color = color.trim();
    if color.is_empty() {
        return Err(invalid_params("a profile needs a color"));
    }
    Ok(color.to_owned())
}

/// The rules on a profile's privacy and models (C9).
fn check_policy(
    private: bool,
    models: &ModelPolicy,
    defaults: &ModelDefaults,
) -> Result<(), RpcError> {
    match models {
        ModelPolicy::Unknown => return Err(invalid_params("unknown model policy")),
        ModelPolicy::Any if private => {
            return Err(invalid_params(
                "a private profile lists its accounts; it can't allow any",
            ));
        }
        _ => {}
    }
    if let Some(provider) = &defaults.provider
        && !models.admits(provider)
    {
        return Err(invalid_params(format!(
            "the default provider {provider:?} isn't one of the profile's accounts"
        )));
    }
    Ok(())
}

/// `profiles.list`.
pub(super) fn list(conn: &Conn) -> Result<ProfilesListResult, RpcError> {
    Ok(ProfilesListResult {
        profiles: conn.hub.repo().profiles().list().map_err(store_error)?,
    })
}

/// `profiles.create`: models default to `any`, or no account for a private profile.
pub(super) fn create(conn: &Conn, p: ProfilesCreateParams) -> Result<Profile, RpcError> {
    let models = p.models.unwrap_or(if p.private {
        ModelPolicy::Only { accounts: vec![] }
    } else {
        ModelPolicy::Any
    });
    check_policy(p.private, &models, &ModelDefaults::default())?;
    let new = NewProfile {
        name: name(&p.name)?,
        color: color(&p.color)?,
        private: p.private,
        models,
    };
    let mut st = conn.hub.lock();
    let profile = conn
        .hub
        .repo()
        .profiles()
        .insert(new)
        .map_err(store_error)?;
    conn.hub.emit_profile(&mut st, &profile);
    Ok(profile)
}

/// `profiles.update`: the result must still satisfy the rules, whichever fields changed.
pub(super) fn update(conn: &Conn, p: ProfilesUpdateParams) -> Result<Profile, RpcError> {
    let hub = &conn.hub;
    let before = hub.profile(p.profile_id)?;
    let changes = ProfileChanges {
        name: p.name.as_deref().map(name).transpose()?,
        color: p.color.as_deref().map(color).transpose()?,
        private: p.private,
        models: p.models,
        model_defaults: p.model_defaults,
        position: p
            .position
            .map(|position| {
                position
                    .is_finite()
                    .then_some(position)
                    .ok_or_else(|| invalid_params("position must be a finite number"))
            })
            .transpose()?,
    };
    let private = changes.private.unwrap_or(before.private);
    check_policy(
        private,
        changes.models.as_ref().unwrap_or(&before.models),
        changes
            .model_defaults
            .as_ref()
            .unwrap_or(&before.model_defaults),
    )?;
    let mut st = hub.lock();
    if private && !before.private {
        let shared = st
            .machines
            .values()
            .find(|m| m.record.profile_ids.len() > 1 && m.record.profile_ids.contains(&before.id));
        if let Some(machine) = shared {
            return Err(policy_denied(
                before.id,
                format!(
                    "{} is shared with another profile; a private profile's machines can't be",
                    machine.record.name
                ),
            ));
        }
    }
    hub.repo()
        .profiles()
        .update(p.profile_id, &changes)
        .map_err(store_error)?;
    let profile = hub.profile(p.profile_id)?;
    hub.emit_profile(&mut st, &profile);
    Ok(profile)
}

/// What a profile still holds, as a phrase ("2 machines and 1 project").
fn describe(contents: ProfileContents) -> String {
    let parts: Vec<String> = [
        (contents.machines, "machine"),
        (contents.projects, "project"),
        (contents.tasks, "task"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}{}", if n == 1 { "" } else { "s" }))
    .collect();
    match parts.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// `profiles.delete`: an empty profile, with its archived sessions (deleted on the agent daemon
/// first, so a failure leaves the profile in place) and its sandbox defaults.
pub(super) async fn delete(conn: &Arc<Conn>, p: ProfilesDeleteParams) -> Result<Ack, RpcError> {
    let hub = &conn.hub;
    let profile = hub.profile(p.profile_id)?;
    let contents = hub
        .repo()
        .profiles()
        .contents(profile.id)
        .map_err(store_error)?;
    if !contents.is_empty() {
        return Err(invalid_state(
            SessionState::Unknown,
            format!(
                "{} still has {}; move or delete them first",
                profile.name,
                describe(contents)
            ),
        ));
    }
    let (open, archived): (Vec<_>, Vec<_>) = {
        let st = hub.lock();
        st.global
            .sessions
            .values()
            .filter(|s| st.session_profile(s) == Some(profile.id))
            .map(|s| (s.id, s.state))
            .partition(|(_, state)| *state != SessionState::Closed)
    };
    if let Some((_, state)) = open.first() {
        return Err(invalid_state(
            *state,
            format!(
                "{} has {} open session(s); archive or move them first",
                profile.name,
                open.len()
            ),
        ));
    }
    if !archived.is_empty() && !p.delete_archived {
        return Err(invalid_state(
            SessionState::Closed,
            format!(
                "{} has {} archived session(s); delete them with it",
                profile.name,
                archived.len()
            ),
        ));
    }
    let archived: Vec<SessionId> = archived.into_iter().map(|(id, _)| id).collect();
    for id in &archived {
        hub.agentd()?
            .call::<SessionDelete>(&SessionOpParams {
                op_id: OpId::generate(),
                id: *id,
            })
            .await
            .map_err(passthrough::agentd_error)?;
    }
    let mut st = hub.lock();
    if hub
        .repo()
        .profiles()
        .delete(profile.id)
        .map_err(store_error)?
    {
        tracing::info!(profile = %profile.id, archived = archived.len(), "profile deleted");
        hub.emit_profile_removed(&mut st, profile.id);
    }
    Ok(Ack {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_profiles_list_their_accounts_and_defaults_stay_within() {
        let none = ModelDefaults::default();
        let only = |a: &[&str]| ModelPolicy::Only {
            accounts: a.iter().map(|s| (*s).to_owned()).collect(),
        };
        assert!(check_policy(false, &ModelPolicy::Any, &none).is_ok());
        assert!(check_policy(true, &ModelPolicy::Any, &none).is_err());
        assert!(check_policy(true, &only(&[]), &none).is_ok());
        assert!(check_policy(false, &ModelPolicy::Unknown, &none).is_err());
        let defaults = ModelDefaults {
            provider: Some("gateway".into()),
            model: None,
            thinking: None,
        };
        assert!(check_policy(true, &only(&["gateway"]), &defaults).is_ok());
        assert!(check_policy(true, &only(&["other"]), &defaults).is_err());
    }

    #[test]
    fn contents_read_as_a_phrase() {
        let c = |machines, projects, tasks| ProfileContents {
            machines,
            projects,
            tasks,
        };
        assert_eq!(describe(c(1, 0, 0)), "1 machine");
        assert_eq!(describe(c(2, 1, 0)), "2 machines and 1 project");
        assert_eq!(describe(c(1, 2, 3)), "1 machine, 2 projects and 3 tasks");
    }
}
