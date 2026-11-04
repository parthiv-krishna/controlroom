//! Routes one host request (after `hello`) to its handler.

use std::sync::Arc;

use controlroom_protocol::host::HostRequest;
use controlroom_protocol::{Ack, ErrorData, Resource, RpcError, RpcRequest};
use serde::Serialize;
use serde_json::Value;

use super::conn::Conn;
use super::store::internal;
use super::{fs_list, tools, workspaces};
use crate::doctor;

fn json<T: Serialize>(result: Result<T, RpcError>) -> Result<Value, RpcError> {
    result.and_then(|value| serde_json::to_value(value).map_err(internal))
}

fn ack(result: Result<(), RpcError>) -> Result<Value, RpcError> {
    json(result.map(|()| Ack {}))
}

fn controller_only(conn: &Conn, method: &str) -> Result<(), RpcError> {
    if conn.host.conns.is_controller(conn.id) {
        Ok(())
    } else {
        Err(RpcError::new(
            ErrorData::Unsupported,
            format!("{method} is for the controller only"),
        ))
    }
}

/// Runs blocking work off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, RpcError> + Send + 'static,
) -> Result<T, RpcError> {
    tokio::task::spawn_blocking(f).await.map_err(internal)?
}

/// Handles `request` and queues its response.
pub async fn handle(conn: &Arc<Conn>, request: RpcRequest<Value>) {
    let id = request.id.clone();
    let call = match HostRequest::from_request(request) {
        Ok(call) => call,
        Err(e) => return conn.respond(id, Err(e)).await,
    };
    let host = &conn.host;
    let result = match call {
        HostRequest::Hello(_) => Err(RpcError::new(
            ErrorData::InvalidRequest,
            "hello was already called on this connection",
        )),
        HostRequest::WorkspaceOpen(p) => json(workspaces::open(host, p).await),
        HostRequest::WorkspaceClose(p) => ack(workspaces::close(host, p).await),
        HostRequest::ShellOpen(p) => json(host.shells.open(host, p).await),
        HostRequest::ShellClose(p) => ack(host.shells.close(host, p).await),
        HostRequest::ShellList(p) => json(Ok(host.shells.list(&p))),
        HostRequest::ToolRun(p) => json(tools::run(host, conn.id, p).await),
        HostRequest::ToolAttach(p) => json(tools::attach(host, conn.id, p).await),
        HostRequest::ToolCancel(p) => json(Ok(tools::cancel(host, &p))),
        HostRequest::JournalAck(p) => ack(tools::ack(host, p).await),
        HostRequest::JobsSubscribe(p) => json(host.jobs.subscribe(host, conn.id, p).await),
        HostRequest::JobsAck(p) => ack(host.jobs.ack(host, p).await),
        HostRequest::ApprovalsSubscribe(_) => controller_only(conn, "approvals.subscribe")
            .and_then(|()| json(Ok(host.approvals.subscribe(conn.id)))),
        HostRequest::ApprovalRespond(p) => {
            controller_only(conn, "approval.respond").and_then(|()| ack(host.approvals.respond(p)))
        }
        HostRequest::Unsubscribe(p) => {
            let id = p.subscription_id;
            if host.jobs.unsubscribe(conn.id, &id) || host.approvals.unsubscribe(conn.id, &id) {
                ack(Ok(()))
            } else {
                Err(RpcError::new(
                    ErrorData::NotFound {
                        resource: Resource::Subscription,
                        id: id.to_string(),
                    },
                    "no such subscription on this connection",
                ))
            }
        }
        HostRequest::FsList(p) => {
            let home = host.paths.dirs().home().clone();
            json(blocking(move || fs_list::list(&home, &p)).await)
        }
        HostRequest::DoctorReportMethod(_) => {
            let dirs = host.paths.dirs().clone();
            json(blocking(move || Ok(doctor::diagnose_host(&dirs))).await)
        }
    };
    conn.respond(id, result).await;
}
