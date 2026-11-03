//! Routes one request to its handler. Params are validated against the method's type first,
//! so malformed params are `invalid_params` before anything happens.

use std::sync::Arc;

use controlroom_protocol::ui::UiRequest;
use controlroom_protocol::{Ack, RpcError, RpcRequest};
use serde_json::Value;

use super::{
    Conn, Reply, board, keys, machines, passthrough, people, reply, sandbox, schedules, sessions,
    state, subscriptions,
};
use crate::errors::internal;

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

        UiRequest::ProjectsCreate(p) => reply(board::projects_create(conn, p)?),
        UiRequest::ProjectsUpdate(p) => reply(board::projects_update(conn, p)?),
        UiRequest::ProjectsDelete(p) => reply(board::projects_delete(conn, p)?),
        UiRequest::TasksCreate(p) => reply(board::tasks_create(conn, p)?),
        UiRequest::TasksUpdate(p) => reply(board::tasks_update(conn, p)?),
        UiRequest::TasksMove(p) => reply(board::tasks_move(conn, p)?),
        UiRequest::TasksDelete(p) => reply(board::tasks_delete(conn, p)?),
        UiRequest::TasksLink(p) => reply(board::tasks_link(conn, p, true)?),
        UiRequest::TasksUnlink(p) => reply(board::tasks_link(conn, p, false)?),
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
        UiRequest::ClientFocus(p) => reply(people::focus(conn, p)),

        UiRequest::SchedulesCreate(p) => reply(schedules::create(conn, p)?),
        UiRequest::SchedulesUpdate(p) => reply(schedules::update(conn, p)?),
        UiRequest::SchedulesDelete(p) => reply(schedules::delete(conn, p)?),
        UiRequest::SchedulesRunNow(p) => reply(schedules::run_now(conn, p).await?),

        UiRequest::SandboxProfiles(_) => reply(sandbox::profiles(conn)?),
        UiRequest::SandboxSetProfile(p) => reply(sandbox::set_profile(conn, p)?),

        // Everything else is in the pass-through table (checked above).
        other => Err(internal(format!("{} is not dispatched", other.method()))),
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
