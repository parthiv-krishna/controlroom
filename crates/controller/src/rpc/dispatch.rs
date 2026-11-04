//! Routes one request of the old UI API to its handler. Params are validated against the
//! method's type first, so malformed params are `invalid_params` before anything happens.

use std::sync::Arc;

use controlroom_protocol::ui::{Priority, ScheduleRef, TaskStatus, UiRequest};
use controlroom_protocol::{Ack, RpcError, RpcRequest};
use serde_json::Value;

use super::{
    Conn, Reply, UiApi, board, keys, machines, passthrough, people, reply, sandbox, schedules,
    sessions, state, subscriptions,
};
use crate::errors::{internal, not_found};
use crate::hub::{Focus, Source, legacy_project};
use crate::store::{NewTask, ScheduleRow, TaskLink, TaskRow};

fn task(row: TaskRow) -> Result<Reply, RpcError> {
    reply(row.legacy())
}

fn schedule(row: ScheduleRow) -> Result<Reply, RpcError> {
    let id = row.id;
    reply(
        row.legacy()
            .ok_or_else(|| not_found(controlroom_protocol::error::Resource::Schedule, id))?,
    )
}

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
        UiRequest::SessionSubscribe(p) => {
            subscriptions::subscribe(conn, Source::Machine(p.machine_id), p.params).await
        }
        UiRequest::Unsubscribe(p) => reply(subscriptions::unsubscribe(conn, p).await?),
        UiRequest::SessionCreate(p) => reply(sessions::create(conn, p).await?),

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

        // Location prefixes are gone: projects group tasks.
        UiRequest::ProjectsCreate(p) => reply(legacy_project(board::projects_create(
            conn, &p.name, &p.color,
        )?)),
        UiRequest::ProjectsUpdate(p) => reply(legacy_project(board::projects_update(
            conn,
            p.project_id,
            p.name.as_deref(),
            p.color.as_deref(),
        )?)),
        UiRequest::ProjectsDelete(p) => reply(board::projects_delete(conn, p)?),
        UiRequest::TasksCreate(p) => task(board::tasks_create(
            conn,
            NewTask {
                title: p.title,
                status: p.status.unwrap_or(TaskStatus::Backlog),
                order: p.order,
                priority: p.priority.unwrap_or(Priority::P2),
                project_ids: p.project_ids,
                notes: p.notes,
                checklist: p.checklist,
                links: p.links.iter().map(TaskLink::legacy).collect(),
            },
        )?),
        UiRequest::TasksUpdate(p) => task(board::tasks_update(conn, p)?),
        UiRequest::TasksMove(p) => task(board::tasks_move(conn, p)?),
        UiRequest::TasksDelete(p) => reply(board::tasks_delete(conn, p)?),
        UiRequest::TasksLink(p) => task(board::tasks_link(
            conn,
            p.task_id,
            TaskLink::legacy(&p.link),
        )?),
        UiRequest::TasksUnlink(p) => task(board::tasks_unlink(conn, p.task_id, p.link.session_id)?),
        UiRequest::TasksChecklistAdd(p) => task(board::checklist_add(conn, p)?),
        UiRequest::TasksChecklistUpdate(p) => task(board::checklist_update(conn, p)?),
        UiRequest::TasksChecklistRemove(p) => task(board::checklist_remove(conn, p)?),

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
                session_id: p.session.map(|s| s.session_id),
                visible: p.visible,
            },
        )),

        UiRequest::SchedulesCreate(p) => schedule(schedules::create(
            conn,
            schedules::Create {
                machine_id: Some(p.machine_id),
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
        UiRequest::SchedulesUpdate(p) => schedule(schedules::update(conn, p, UiApi::Legacy)?),
        UiRequest::SchedulesDelete(p) => reply(schedules::delete(conn, p, UiApi::Legacy)?),
        UiRequest::SchedulesRunNow(p) => run_now(conn, p).await,

        UiRequest::SandboxProfiles(_) => reply(sandbox::profiles(conn)?),
        UiRequest::SandboxSetProfile(p) => reply(sandbox::set_profile(conn, p)?),

        // Everything else is in the pass-through table (checked above).
        other => Err(internal(format!("{} is not dispatched", other.method()))),
    }
}

async fn run_now(conn: &Arc<Conn>, p: ScheduleRef) -> Result<Reply, RpcError> {
    schedule(schedules::run_now(conn, p, UiApi::Legacy).await?)
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
        ];
        let namespaces = [
            "machines.",
            "keys.",
            "projects.",
            "tasks.",
            "grants.",
            "devices.",
            "notifications.",
            "client.",
            "schedules.",
            "sandbox.profiles",
            "sandbox.set_profile",
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
