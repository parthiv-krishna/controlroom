//! Routes one request (after `hello`) to the runtime or to the daemon's own handlers.
//!
//! Session, queue, request, todo, job and settings commands go to the runtime (a `job.kill`
//! here is the user's). `agents.list`, `fs.list` and `doctor.report` run here on blocking
//! threads. Features of later phases (`sandbox.revoke`, `outbox.*`, `inbox.deliver`) answer
//! `unsupported`.

use std::sync::Arc;

use controlroom_protocol::daemon::{AgentsListResult, DaemonRequest};
use controlroom_protocol::{Ack, ErrorData, Resource, RpcError, RpcRequest};
use serde::Serialize;
use serde_json::Value;

use super::conn::Conn;
use super::{feeds, fs_list};
use crate::config::agents as agents_config;
use crate::doctor;
use crate::shell::login_shell;

/// An `internal_error`.
pub fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorData::InternalError, e.to_string())
}

fn json<T: Serialize>(result: Result<T, RpcError>) -> Result<Value, RpcError> {
    result.and_then(|value| serde_json::to_value(value).map_err(internal))
}

fn ack(result: Result<(), RpcError>) -> Result<Value, RpcError> {
    json(result.map(|()| Ack {}))
}

fn unsupported(method: &str) -> Result<Value, RpcError> {
    Err(RpcError::new(
        ErrorData::Unsupported,
        format!("{method} is not available in this daemon version"),
    ))
}

/// Runs blocking work off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, RpcError> + Send + 'static,
) -> Result<T, RpcError> {
    tokio::task::spawn_blocking(f).await.map_err(internal)?
}

/// Handles `request` and queues its response (subscriptions queue theirs before their feed).
pub async fn handle(conn: &Arc<Conn>, request: RpcRequest<Value>) {
    let id = request.id.clone();
    let call = match DaemonRequest::from_request(request) {
        Ok(call) => call,
        Err(e) => return conn.respond(id, Err(e)).await,
    };
    let method = call.method();
    let runtime = &conn.shared.runtime;
    let result = match call {
        DaemonRequest::Hello(_) => Err(RpcError::new(
            ErrorData::InvalidRequest,
            "hello was already called on this connection",
        )),
        DaemonRequest::MachineSubscribe(p) => return feeds::subscribe_machine(conn, id, p).await,
        DaemonRequest::SessionSubscribe(p) => return feeds::subscribe_session(conn, id, p).await,
        DaemonRequest::Unsubscribe(p) => {
            if conn.remove_subscription(&p.subscription_id) {
                ack(Ok(()))
            } else {
                Err(RpcError::new(
                    ErrorData::NotFound {
                        resource: Resource::Subscription,
                        id: p.subscription_id.to_string(),
                    },
                    "no such subscription on this connection",
                ))
            }
        }

        DaemonRequest::SessionCreate(p) => json(runtime.create(p).await),
        DaemonRequest::SessionPrompt(p) => json(runtime.prompt(p).await),
        DaemonRequest::SessionCancel(p) => ack(runtime.cancel(p).await),
        DaemonRequest::SessionClose(p) => ack(runtime.close(p).await),
        DaemonRequest::SessionReopen(p) => ack(runtime.reopen(p).await),
        DaemonRequest::SessionResume(p) => ack(runtime.resume(p).await),
        DaemonRequest::SessionStartFresh(p) => json(runtime.start_fresh(p).await),
        DaemonRequest::SessionDelete(p) => ack(runtime.delete(p).await),
        DaemonRequest::SessionUpdate(p) => ack(runtime.update(p).await),
        DaemonRequest::SessionMarkRead(p) => json(runtime.mark_read(p).await),
        DaemonRequest::SessionMarkUnread(p) => json(runtime.mark_unread(p).await),
        DaemonRequest::SessionSetOption(p) => ack(runtime.set_option(p).await),

        DaemonRequest::QueueSendNow(p) => json(runtime.send_now(p).await),
        DaemonRequest::QueueEdit(p) => ack(runtime.queue_edit(p).await),
        DaemonRequest::QueueRemove(p) => ack(runtime.queue_remove(p).await),
        DaemonRequest::QueueReorder(p) => ack(runtime.queue_reorder(p).await),
        DaemonRequest::QueueResume(p) => ack(runtime.queue_resume(p).await),

        DaemonRequest::RequestRespond(p) => json(runtime.respond(p).await),

        DaemonRequest::JobList(p) => json(runtime.job_list(p).await),
        DaemonRequest::JobLogsMethod(p) => json(runtime.job_logs(p).await),
        DaemonRequest::JobKill(p) => json(runtime.job_kill(p).await),
        DaemonRequest::JobFollowMethod(p) => json(runtime.job_follow(p).await),
        DaemonRequest::JobUnfollow(p) => json(runtime.job_unfollow(p).await),

        DaemonRequest::AgentsList(_) => {
            let dirs = conn.shared.dirs.clone();
            json(
                blocking(move || {
                    let agents = agents_config::load(&dirs).value;
                    Ok(AgentsListResult {
                        agents: doctor::check_agents(
                            &login_shell(),
                            &std::env::var("PATH").unwrap_or_default(),
                            &agents,
                        ),
                    })
                })
                .await,
            )
        }
        DaemonRequest::FsList(p) => {
            let home = conn.shared.dirs.home().clone();
            json(blocking(move || fs_list::list(&home, &p)).await)
        }
        DaemonRequest::DoctorReportMethod(_) => {
            let dirs = conn.shared.dirs.clone();
            json(blocking(move || Ok(doctor::diagnose(&dirs).report)).await)
        }
        DaemonRequest::SettingsGet(_) => json(Ok(runtime.settings())),
        DaemonRequest::SettingsSet(p) => json(runtime.set_settings(p).await),

        DaemonRequest::TodoAdd(p) => json(runtime.todo_add(p).await),
        DaemonRequest::TodoUpdate(p) => json(runtime.todo_update(p).await),
        DaemonRequest::TodoRemove(p) => ack(runtime.todo_remove(p).await),
        DaemonRequest::TodoReorder(p) => ack(runtime.todo_reorder(p).await),

        DaemonRequest::SandboxRevoke(_)
        | DaemonRequest::OutboxSubscribe(_)
        | DaemonRequest::OutboxAck(_)
        | DaemonRequest::InboxDeliver(_) => unsupported(method),
    };
    conn.respond(id, result).await;
}
