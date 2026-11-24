//! Routes one UI request to its handler after validating its params against the method's type,
//! so malformed params are `invalid_params` before anything happens.
//!
//! Session commands pass through to the agent daemon ([`passthrough`]); `session.create`
//! does more (profile, sandbox defaults, attribution, task links); `fs.list` and a machine's
//! `doctor.report` go to that machine's host daemon.

use std::sync::Arc;

use controlroom_client_core::daemon::{CallOptions, CallTimeout};
use controlroom_protocol::agentd::SessionCreateResult;
use controlroom_protocol::agentd::methods::SessionCreate;
use controlroom_protocol::error::Resource;
use controlroom_protocol::host::methods::{DoctorReportMethod, FsList};
use controlroom_protocol::host::{DoctorReport, FsListParams, FsListResult};
use controlroom_protocol::ids::{MachineId, ProfileId};
use controlroom_protocol::session::PromptOrigin;
use controlroom_protocol::ui::{
    DoctorReportParams, OnMachine, Profile, TaskStatus, UiRequest, UiSessionCreateParams,
};
use controlroom_protocol::{Ack, NoParams, RpcError, RpcRequest};
use serde_json::Value;

use super::{
    Conn, Reply, board, keys, machines, membership, passthrough, people, profile_move, profiles,
    reply, sandbox, schedules, state, subscriptions,
};
use crate::errors::{call_error, internal, invalid_params, not_found, store_error};
use crate::hub::{Focus, Hub};

pub(super) async fn dispatch(
    conn: &Arc<Conn>,
    request: RpcRequest<Value>,
) -> Result<Reply, RpcError> {
    let RpcRequest { method, params, .. } = request;
    let call = UiRequest::from_parts(&method, params.clone())?;
    if passthrough::is_passed_through(&method) {
        return passthrough::pass(conn, &method, params).await;
    }
    match call {
        UiRequest::Hello(p) => reply(state::hello(conn, p)?),
        UiRequest::Ping(_) => reply(Ack {}),
        UiRequest::StateSubscribe(p) => state::subscribe(conn, p),
        UiRequest::SessionSubscribe(p) => subscriptions::subscribe(conn, p).await,
        UiRequest::Unsubscribe(p) => reply(subscriptions::unsubscribe(conn, p).await?),
        UiRequest::SessionCreate(p) => reply(create(conn, p).await?),

        UiRequest::MachinesAdd(p) => reply(machines::add(conn, p).await?),
        UiRequest::MachinesAddConfirm(p) => reply(machines::add_confirm(conn, p).await?),
        UiRequest::MachinesConfirmHostKey(p) => reply(machines::confirm_host_key(conn, p).await?),
        UiRequest::MachinesRemove(p) => reply(machines::remove(conn, p).await?),
        UiRequest::MachinesReconnect(p) => reply(machines::reconnect(conn, p)?),
        UiRequest::MachinesUpgrade(p) => reply(machines::upgrade(conn, p)?),
        UiRequest::MachinesSetSettings(p) => reply(machines::set_settings(conn, p)?),

        UiRequest::KeysList(_) => reply(keys::list(conn)?),
        UiRequest::KeysRotate(_) => reply(crate::rotation::rotate(&conn.hub).await?),
        UiRequest::KeysGenerate(p) => reply(keys::generate(conn, p)?),
        UiRequest::KeysImport(p) => reply(keys::import(conn, p)?),
        UiRequest::KeysDelete(p) => reply(keys::delete(conn, p)?),

        UiRequest::ProfilesList(_) => reply(profiles::list(conn)?),
        UiRequest::ProfilesCreate(p) => reply(profiles::create(conn, p)?),
        UiRequest::ProfilesUpdate(p) => reply(profiles::update(conn, p)?),
        UiRequest::ProfilesDelete(p) => reply(profiles::delete(conn, p).await?),
        UiRequest::ProfilesMove(p) => reply(profile_move::run(conn, p).await?),

        UiRequest::ProjectsCreate(p) => reply(board::projects_create(conn, p)?),
        UiRequest::ProjectsUpdate(p) => reply(board::projects_update(
            conn,
            p.project_id,
            p.name.as_deref(),
            p.color.as_deref(),
        )?),
        UiRequest::ProjectsDelete(p) => reply(board::projects_delete(conn, p)?),
        UiRequest::TasksCreate(p) => reply(board::tasks_create(conn, p)?),
        UiRequest::TasksUpdate(p) => reply(board::tasks_update(conn, p)?),
        UiRequest::TasksMove(p) => reply(board::tasks_move(conn, p)?),
        UiRequest::TasksDelete(p) => reply(board::tasks_delete(conn, p)?),
        UiRequest::TasksLink(p) => reply(board::tasks_link(conn, p.task_id, p.session_id)?),
        UiRequest::TasksUnlink(p) => reply(board::tasks_unlink(conn, p.task_id, p.session_id)?),
        UiRequest::TasksChecklistAdd(p) => reply(board::checklist_add(conn, p)?),
        UiRequest::TasksChecklistUpdate(p) => reply(board::checklist_update(conn, p)?),
        UiRequest::TasksChecklistRemove(p) => reply(board::checklist_remove(conn, p)?),

        UiRequest::GrantsRevoke(p) => reply(people::grants_revoke(conn, p)?),
        UiRequest::DevicesRegister(p) => reply(people::devices_register(conn, p)?),
        UiRequest::DevicesRename(p) => reply(people::devices_rename(conn, p)?),
        UiRequest::DevicesRemove(p) => reply(people::devices_remove(conn, p)?),
        UiRequest::NotificationsPreferences(_) => reply(people::preferences(conn)?),
        UiRequest::NotificationsSetPreferences(p) => reply(people::set_preferences(conn, p)?),
        UiRequest::NotificationsSubscribe(p) => reply(people::push_subscribe(conn, p)?),
        UiRequest::NotificationsUnsubscribe(p) => reply(people::push_unsubscribe(conn, p)?),
        UiRequest::ClientFocus(p) => reply(people::focus(
            conn,
            Focus {
                device_id: p.device_id,
                session_id: p.session_id,
                visible: p.visible,
            },
        )),
        UiRequest::ClientActivity(_) => reply(people::activity(conn)),

        UiRequest::SchedulesCreate(p) => reply(schedules::create(
            conn,
            schedules::Create {
                session_id: p.session_id,
                name: p.name,
                spec: p.spec,
                timezone: p.timezone,
                message: p.message,
                enabled: p.enabled,
                if_busy: p.if_busy,
                missed: p.missed,
            },
        )?),
        UiRequest::SchedulesUpdate(p) => reply(schedules::update(conn, p)?),
        UiRequest::SchedulesDelete(p) => reply(schedules::delete(conn, p)?),
        UiRequest::SchedulesRunNow(p) => reply(schedules::run_now(conn, p).await?),

        UiRequest::SandboxDefaultsMethod(_) => reply(sandbox::list(conn)?),
        UiRequest::SandboxSetDefaults(p) => reply(sandbox::set(conn, p)?),

        UiRequest::FsList(p) => reply(fs_list(conn, p).await?),
        UiRequest::DoctorReportMethod(p) => reply(doctor(conn, p).await?),

        // Everything else is in the pass-through table (checked above).
        other => Err(internal(format!("{} is not dispatched", other.method()))),
    }
}

/// `session.create`: the agent daemon creates the session on its workspaces with its first
/// prompt (atomically, under the `op_id`), then the controller links it to the tasks, moving
/// tasks in `backlog` or `todo` to `doing`. The controller fills in the profile (C1: every
/// machine and task must be in it; omitted, the one the machines name), the sandbox defaults
/// (the profile's, the workspaces' machines' and the tasks' projects'; level off is refused on
/// a shared machine, C8) and the prompt's origin (this connection's device), replacing whatever
/// a client sent. Links are set-like, so a retry is safe.
async fn create(
    conn: &Arc<Conn>,
    p: UiSessionCreateParams,
) -> Result<SessionCreateResult, RpcError> {
    let hub = &conn.hub;
    for task in &p.task_ids {
        hub.task(*task)?;
    }
    let mut create = *p.create;
    let mut machines = Vec::new();
    for w in &create.workspaces {
        hub.machine_view(w.machine_id)?;
        if !machines.contains(&w.machine_id) {
            machines.push(w.machine_id);
        }
    }
    let profile = session_profile(hub, create.profile_id, &machines)?;
    membership::check_machines_in(hub, &profile, &machines)?;
    membership::check_tasks_in(hub, &profile, &p.task_ids)?;
    let sandbox = sandbox::defaults(hub, Some(profile.id), &machines, &p.task_ids)?;
    let level = create.sandbox_level.unwrap_or(sandbox.level);
    membership::check_level(hub, profile.id, &machines, level)?;
    create.profile_id = Some(profile.id);
    create.sandbox = Some(sandbox);
    if let Some(prompt) = &mut create.prompt {
        prompt.origin = PromptOrigin::User {
            device: conn.device_ref(),
        };
    }
    // The agent daemon refuses machines it doesn't see connected: let it see what the UI saw.
    hub.outbound.barrier().await;
    let created = hub
        .agentd()?
        .call::<SessionCreate>(&create)
        .await
        .map_err(passthrough::agentd_error)?;
    let session = created.session.id;
    for task in &p.task_ids {
        let mut st = hub.lock();
        let repo = hub.repo();
        let tasks = repo.tasks();
        if !tasks.link(*task, session).map_err(store_error)? {
            return Err(not_found(Resource::Task, task));
        }
        tasks
            .move_if_in(
                *task,
                &[TaskStatus::Backlog, TaskStatus::Todo],
                TaskStatus::Doing,
            )
            .map_err(store_error)?;
        let row = hub.task(*task)?;
        hub.emit_task(&mut st, &row);
    }
    Ok(created)
}

/// A new session's profile: the one given, else the one profile its machines share.
fn session_profile(
    hub: &Hub,
    id: Option<ProfileId>,
    machines: &[MachineId],
) -> Result<Profile, RpcError> {
    if let Some(id) = id {
        return hub.profile(id);
    }
    membership::require_profile(hub)?;
    let common: Vec<ProfileId> = {
        let st = hub.lock();
        let mut common = machines
            .first()
            .map(|m| st.machine_profiles(*m).to_vec())
            .unwrap_or_default();
        for machine in machines {
            let here = st.machine_profiles(*machine);
            common.retain(|p| here.contains(p));
        }
        common
    };
    match common.as_slice() {
        [only] => hub.profile(*only),
        // No workspace yet: the agent daemon refuses it; the only profile, if there is one.
        [] if machines.is_empty() => membership::profile_or_only(hub, None),
        _ => Err(invalid_params(
            "profile_id is required: the machines don't name exactly one profile",
        )),
    }
}

/// `fs.list`: a directory on a machine, listed by its host daemon with the user's own access.
async fn fs_list(conn: &Conn, p: OnMachine<FsListParams>) -> Result<FsListResult, RpcError> {
    let hub = &conn.hub;
    let machine = p.machine_id;
    let options = CallOptions {
        timeout: CallTimeout::After(hub.services.tuning.slow_call_timeout),
        ..CallOptions::default()
    };
    hub.host(machine)?
        .call_with::<FsList>(&p.params, options)
        .await
        .map_err(|e| call_error(e, machine, || hub.connectivity(machine)))
}

/// `doctor.report`: a machine's host daemon checks (stored on the machine), or the server's
/// (the agent daemon's).
async fn doctor(conn: &Conn, p: DoctorReportParams) -> Result<DoctorReport, RpcError> {
    let hub = &conn.hub;
    let options = CallOptions {
        timeout: CallTimeout::After(hub.services.tuning.slow_call_timeout),
        ..CallOptions::default()
    };
    match p.machine_id {
        Some(machine) => {
            let report = hub
                .host(machine)?
                .call_with::<DoctorReportMethod>(&NoParams {}, options)
                .await
                .map_err(|e| call_error(e, machine, || hub.connectivity(machine)))?;
            hub.set_doctor(machine, &report);
            Ok(report)
        }
        None => hub
            .agentd()?
            .call_with::<controlroom_protocol::agentd::methods::DoctorReportMethod>(
                &NoParams {},
                options,
            )
            .await
            .map_err(passthrough::agentd_error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The handlers above and the pass-through table must cover every UI method.
    #[test]
    fn every_ui_method_is_handled_or_passed_through() {
        let handled = [
            "hello",
            "ping",
            "state.subscribe",
            "session.subscribe",
            "unsubscribe",
            "session.create",
            "fs.list",
            "doctor.report",
        ];
        let namespaces = [
            "profiles.",
            "machines.",
            "keys.",
            "projects.",
            "tasks.",
            "grants.",
            "devices.",
            "notifications.",
            "client.",
            "schedules.",
            "sandbox.defaults",
            "sandbox.set_defaults",
        ];
        for method in UiRequest::METHODS {
            let ours = handled.contains(method) || namespaces.iter().any(|n| method.starts_with(n));
            assert!(
                ours != passthrough::is_passed_through(method),
                "{method} must be either handled or passed through"
            );
        }
    }
}
