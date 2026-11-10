//! Routes one request (after `hello`) to the runtime or to the agent daemon's own handlers.
//!
//! Session, workspace, queue, request, todo, job and settings commands go to the runtime (a
//! `job.kill` here is the user's). The host bridge (`hostops.*`) is the controller's only;
//! debug clients get `unsupported`. `agents.list` and `doctor.report` run here on blocking
//! threads. `models.*` go to Settings › Models ([`crate::models`]). `sandbox.revoke` (session grants) answers `unsupported` until grants land.

use std::sync::Arc;

use controlroom_protocol::agentd::{AgentdRequest, AgentsListResult};
use controlroom_protocol::{Ack, ErrorData, Resource, RpcError, RpcRequest};
use serde::Serialize;
use serde_json::Value;

use super::conn::Conn;
use super::feeds;
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

/// Runs blocking work off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, RpcError> + Send + 'static,
) -> Result<T, RpcError> {
    tokio::task::spawn_blocking(f).await.map_err(internal)?
}

/// Handles `request` and queues its response (subscriptions queue theirs before their feed).
pub async fn handle(conn: &Arc<Conn>, request: RpcRequest<Value>) {
    let id = request.id.clone();
    let call = match AgentdRequest::from_request(request) {
        Ok(call) => call,
        Err(e) => return conn.respond(id, Err(e)).await,
    };
    let method = call.method();
    if method.starts_with("hostops.") && !conn.is_controller() {
        let error = RpcError::new(
            ErrorData::Unsupported,
            format!("{method} is for the controller only"),
        );
        return conn.respond(id, Err(error)).await;
    }
    let runtime = &conn.shared.runtime;
    let models = &conn.shared.models;
    let result = match call {
        AgentdRequest::Hello(_) => Err(RpcError::new(
            ErrorData::InvalidRequest,
            "hello was already called on this connection",
        )),
        AgentdRequest::SessionsSubscribe(p) => {
            return feeds::subscribe_sessions(conn, id, p).await;
        }
        AgentdRequest::SessionSubscribe(p) => return feeds::subscribe_session(conn, id, p).await,
        AgentdRequest::HostOpsSubscribe(_) => return feeds::subscribe_hostops(conn, id).await,
        AgentdRequest::Unsubscribe(p) => {
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

        AgentdRequest::SessionCreate(p) => json(runtime.create(p).await),
        AgentdRequest::SessionWorkspaceAdd(p) => json(runtime.workspace_add(p).await),
        AgentdRequest::SessionWorkspaceRemove(p) => ack(runtime.workspace_remove(p).await),
        AgentdRequest::SessionPrompt(p) => json(runtime.prompt(p).await),
        AgentdRequest::SessionCancel(p) => ack(runtime.cancel(p).await),
        AgentdRequest::SessionClose(p) => ack(runtime.close(p).await),
        AgentdRequest::SessionReopen(p) => ack(runtime.reopen(p).await),
        AgentdRequest::SessionResume(p) => ack(runtime.resume(p).await),
        AgentdRequest::SessionStartFresh(p) => json(runtime.start_fresh(p).await),
        AgentdRequest::SessionDelete(p) => ack(runtime.delete(p).await),
        AgentdRequest::SessionUpdate(p) => ack(runtime.update(p).await),
        AgentdRequest::SessionMarkRead(p) => json(runtime.mark_read(p).await),
        AgentdRequest::SessionMarkUnread(p) => json(runtime.mark_unread(p).await),
        AgentdRequest::SessionSetOption(p) => ack(runtime.set_option(p).await),
        AgentdRequest::SessionSetSandboxLevel(p) => ack(runtime.set_sandbox_level(p).await),
        AgentdRequest::SessionTerminalOpen(p) => json(runtime.terminal_open(p).await),
        AgentdRequest::SessionTerminalClose(p) => ack(runtime.terminal_close(p).await),

        AgentdRequest::QueueSendNow(p) => json(runtime.send_now(p).await),
        AgentdRequest::QueueEdit(p) => ack(runtime.queue_edit(p).await),
        AgentdRequest::QueueRemove(p) => ack(runtime.queue_remove(p).await),
        AgentdRequest::QueueReorder(p) => ack(runtime.queue_reorder(p).await),
        AgentdRequest::QueueResume(p) => ack(runtime.queue_resume(p).await),

        AgentdRequest::RequestRespond(p) => json(runtime.respond(p).await),

        AgentdRequest::TodoAdd(p) => json(runtime.todo_add(p).await),
        AgentdRequest::TodoUpdate(p) => json(runtime.todo_update(p).await),
        AgentdRequest::TodoRemove(p) => ack(runtime.todo_remove(p).await),
        AgentdRequest::TodoReorder(p) => ack(runtime.todo_reorder(p).await),

        AgentdRequest::JobList(p) => json(runtime.job_list(p).await),
        AgentdRequest::JobLogsMethod(p) => json(runtime.job_logs(p).await),
        AgentdRequest::JobKill(p) => json(runtime.job_kill(p).await),
        AgentdRequest::JobFollowMethod(p) => json(runtime.job_follow(p).await),
        AgentdRequest::JobUnfollow(p) => json(runtime.job_unfollow(p).await),

        AgentdRequest::SandboxRevoke(_) => Err(RpcError::new(
            ErrorData::Unsupported,
            format!("{method} is not available in this version"),
        )),

        AgentdRequest::AgentsList(_) => {
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
        AgentdRequest::DoctorReportMethod(_) => {
            let dirs = conn.shared.dirs.clone();
            json(blocking(move || Ok(doctor::diagnose(&dirs))).await)
        }
        AgentdRequest::SettingsGet(_) => json(Ok(runtime.settings())),
        AgentdRequest::SettingsSet(p) => json(runtime.set_settings(p).await),

        AgentdRequest::ModelsList(_) => json(models.list().await),
        AgentdRequest::ModelsSetKey(p) => json(models.set_key(p).await),
        AgentdRequest::ModelsSetGateway(p) => json(models.set_gateway(p).await),
        AgentdRequest::ModelsRemove(p) => json(models.remove(p).await),
        AgentdRequest::ModelsSignOut(p) => json(models.sign_out(p).await),
        AgentdRequest::ModelsTest(p) => json(models.test(p).await),
        AgentdRequest::ModelsSetDefaults(p) => json(models.set_defaults(p).await),
        AgentdRequest::ModelsLoginStart(p) => json(models.login_start(p).await),
        AgentdRequest::ModelsLoginInput(p) => json(models.login_input(p).await),
        AgentdRequest::ModelsLoginWait(p) => json(models.login_wait(p).await),
        AgentdRequest::ModelsLoginCancel(p) => json(models.login_cancel(p).await),

        AgentdRequest::HostOpsRespond(p) => {
            runtime.hostops_respond(&p.id, p.outcome);
            ack(Ok(()))
        }
        AgentdRequest::HostOpsNotify(p) => {
            runtime.hostops_notify(p.machine_id, p.notification);
            ack(Ok(()))
        }
        AgentdRequest::HostOpsMachines(p) => ack(runtime.hostops_machines(p.machines)),
    };
    conn.respond(id, result).await;
}
