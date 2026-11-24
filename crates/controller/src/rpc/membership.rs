//! Profile membership rules the controller enforces (design "profiles as tenants" §2, C1–C8):
//! which profile a new machine entry, project, task or session belongs to, that everything a
//! session or task touches is in its profile, and that a shared machine entry never runs at
//! level off. Refusals are `policy_denied` with a message that names only the profile the rule
//! is about.

use controlroom_protocol::RpcError;
use controlroom_protocol::ids::{MachineId, ProfileId, SessionId, TaskId};
use controlroom_protocol::session::{SandboxLevel, SessionState};
use controlroom_protocol::ui::{DefaultsScope, Profile, Task};

use crate::errors::{invalid_params, policy_denied, store_error};
use crate::hub::Hub;

/// The answer while no profile exists.
const CREATE_A_PROFILE_FIRST: &str = "create a profile first";

/// The profile something new goes to: `id` (which must exist), or the only profile when it is
/// omitted. With none, "create a profile first"; with several, the id is required.
pub(super) fn profile_or_only(hub: &Hub, id: Option<ProfileId>) -> Result<Profile, RpcError> {
    if let Some(id) = id {
        return hub.profile(id);
    }
    let mut profiles = hub.repo().profiles().list().map_err(store_error)?;
    match profiles.len() {
        0 => Err(invalid_params(CREATE_A_PROFILE_FIRST)),
        1 => Ok(profiles.remove(0)),
        _ => Err(invalid_params(
            "profile_id is required with several profiles",
        )),
    }
}

/// Refuses while no profile exists (a fresh install): "create a profile first".
pub(super) fn require_profile(hub: &Hub) -> Result<(), RpcError> {
    if hub
        .repo()
        .profiles()
        .list()
        .map_err(store_error)?
        .is_empty()
    {
        return Err(invalid_params(CREATE_A_PROFILE_FIRST));
    }
    Ok(())
}

/// The profiles of a new or changed machine entry: `ids`, or the only profile when empty
/// (C7). Each must exist, and a private profile is never on a shared entry.
pub(super) fn machine_profiles(hub: &Hub, ids: &[ProfileId]) -> Result<Vec<ProfileId>, RpcError> {
    if ids.is_empty() {
        return Ok(vec![profile_or_only(hub, None)?.id]);
    }
    let mut profiles: Vec<Profile> = Vec::new();
    for id in ids {
        if !profiles.iter().any(|p| p.id == *id) {
            profiles.push(hub.profile(*id)?);
        }
    }
    if profiles.len() > 1
        && let Some(private) = profiles.iter().find(|p| p.private)
    {
        return Err(policy_denied(
            private.id,
            format!("{} is private; its machines can't be shared", private.name),
        ));
    }
    Ok(profiles.into_iter().map(|p| p.id).collect())
}

/// Refuses a machine that isn't in `profile` (C1, C2).
pub(super) fn check_machines_in(
    hub: &Hub,
    profile: &Profile,
    machines: &[MachineId],
) -> Result<(), RpcError> {
    let st = hub.lock();
    for machine in machines {
        if !st.machine_profiles(*machine).contains(&profile.id) {
            let name = st
                .machines
                .get(machine)
                .map_or_else(|| machine.to_string(), |e| e.record.name.clone());
            return Err(policy_denied(
                profile.id,
                format!("{name} isn't in {}", profile.name),
            ));
        }
    }
    Ok(())
}

/// Refuses level off when any of `machines` is a shared entry (C8): the sandbox is then the only
/// thing between the profiles that share it.
pub(super) fn check_level(
    hub: &Hub,
    profile: ProfileId,
    machines: &[MachineId],
    level: SandboxLevel,
) -> Result<(), RpcError> {
    if level != SandboxLevel::Off {
        return Ok(());
    }
    let st = hub.lock();
    for machine in machines {
        if st.machine_profiles(*machine).len() > 1 {
            let name = st
                .machines
                .get(machine)
                .map_or_else(|| machine.to_string(), |e| e.record.name.clone());
            return Err(policy_denied(
                profile,
                format!("{name} is shared with another profile; it can't run unsandboxed"),
            ));
        }
    }
    Ok(())
}

/// Refuses sharing a machine entry that runs at level off (C8): its own defaults' level, or an
/// open session on it.
pub(super) fn check_shareable(
    hub: &Hub,
    profile: ProfileId,
    machine: MachineId,
) -> Result<(), RpcError> {
    let defaults = hub.repo().sandbox_defaults().list().map_err(store_error)?;
    let off_defaults = defaults.iter().any(|d| {
        d.level == Some(SandboxLevel::Off)
            && d.scope
                == DefaultsScope::Machine {
                    machine_id: machine,
                }
    });
    let st = hub.lock();
    let off_session = st.global.sessions.values().any(|s| {
        s.state != SessionState::Closed
            && s.sandbox.level == SandboxLevel::Off
            && s.workspaces.iter().any(|w| w.machine_id == machine)
    });
    if off_defaults || off_session {
        let name = st
            .machines
            .get(&machine)
            .map_or_else(|| machine.to_string(), |e| e.record.name.clone());
        return Err(policy_denied(
            profile,
            format!("{name} runs unsandboxed; a shared machine can't"),
        ));
    }
    Ok(())
}

/// The profile of a session the controller knows, if it can tell.
pub(super) fn session_profile(hub: &Hub, session: SessionId) -> Option<ProfileId> {
    let st = hub.lock();
    let summary = st.global.sessions.get(&session)?;
    st.session_profile(summary)
}

/// Refuses a session of another profile than `profile`, e.g. linking it to a task (C4: a link
/// grants contact). A session the controller can't place is left alone (links may dangle).
pub(super) fn check_session_in(
    hub: &Hub,
    profile: &Profile,
    session: SessionId,
) -> Result<(), RpcError> {
    match session_profile(hub, session) {
        Some(other) if other != profile.id => Err(policy_denied(
            profile.id,
            format!("that session isn't in {}", profile.name),
        )),
        _ => Ok(()),
    }
}

/// The profile of a stored task (every task has one).
pub(super) fn task_profile(hub: &Hub, task: &Task) -> Result<Profile, RpcError> {
    profile_or_only(hub, task.profile_id)
}

/// Refuses tasks outside `profile` (C1, C4).
pub(super) fn check_tasks_in(
    hub: &Hub,
    profile: &Profile,
    tasks: &[TaskId],
) -> Result<(), RpcError> {
    for task in tasks {
        let task = hub.task(*task)?;
        if task.profile_id != Some(profile.id) {
            return Err(policy_denied(
                profile.id,
                format!("the task {:?} isn't in {}", task.title, profile.name),
            ));
        }
    }
    Ok(())
}
