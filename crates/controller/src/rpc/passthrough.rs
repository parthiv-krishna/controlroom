//! Table-driven pass-through of session-scoped methods to the agent daemon.
//!
//! The params are the agent daemon's, unchanged. The controller fills in what only it knows
//! (per [`Fill`]) and forwards the call unchanged otherwise: `op_id`s pass through as they
//! are, and results and errors come back unchanged. While the agent daemon can't be reached
//! the call fails with `timeout`; nothing is queued.

use std::sync::Arc;

use controlroom_client_core::daemon::CallError;
use controlroom_protocol::ids::SessionId;
use controlroom_protocol::session::PromptOrigin;
use controlroom_protocol::{ErrorData, RpcError};
use serde_json::{Map, Value};

use super::{Conn, Reply, sandbox};
use crate::errors::{internal, invalid_params, store_error};
use crate::hub::agentd_unavailable;

/// What the controller fills in before forwarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fill {
    /// Nothing.
    None,
    /// `origin`: the user on this connection's device (replacing any value sent).
    PromptOrigin,
    /// `sandbox`: the session's refreshed defaults (its machines and its tasks' projects).
    Sandbox,
    /// `by`: the deciding (or editing) device is this connection's, always (a client's value
    /// is dropped).
    Responder,
}

/// One passed-through method.
struct Entry {
    method: &'static str,
    fill: Fill,
    /// Needs the agent daemon to know the machines as the UI shows them (it refuses a machine
    /// it doesn't see connected): waits for the queued machine reports first.
    machines: bool,
}

const fn plain(method: &'static str) -> Entry {
    Entry {
        method,
        fill: Fill::None,
        machines: false,
    }
}

const fn fill(method: &'static str, fill: Fill) -> Entry {
    Entry {
        fill,
        ..plain(method)
    }
}

/// Every passed-through method.
const TABLE: &[Entry] = &[
    fill("session.prompt", Fill::PromptOrigin),
    Entry {
        machines: true,
        ..plain("session.workspace_add")
    },
    plain("session.workspace_remove"),
    plain("session.cancel"),
    plain("session.close"),
    plain("session.reopen"),
    fill("session.resume", Fill::Sandbox),
    fill("session.start_fresh", Fill::Sandbox),
    plain("session.delete"),
    plain("session.update"),
    plain("session.mark_read"),
    plain("session.mark_unread"),
    plain("session.set_option"),
    plain("queue.send_now"),
    plain("queue.edit"),
    plain("queue.remove"),
    plain("queue.reorder"),
    plain("queue.resume"),
    fill("request.respond", Fill::Responder),
    fill("todo.add", Fill::Responder),
    fill("todo.update", Fill::Responder),
    fill("todo.remove", Fill::Responder),
    fill("todo.reorder", Fill::Responder),
    plain("job.list"),
    plain("job.logs"),
    plain("job.kill"),
    plain("job.follow"),
    plain("job.unfollow"),
    plain("sandbox.revoke"),
    plain("agents.list"),
    plain("settings.get"),
    plain("settings.set"),
];

/// Whether `method` is passed through to the agent daemon.
pub(crate) fn is_passed_through(method: &str) -> bool {
    TABLE.iter().any(|e| e.method == method)
}

fn entry(method: &str) -> Result<&'static Entry, RpcError> {
    TABLE
        .iter()
        .find(|e| e.method == method)
        .ok_or_else(|| internal(format!("{method} is not passed through")))
}

fn object(params: Value) -> Result<Map<String, Value>, RpcError> {
    match params {
        Value::Object(params) => Ok(params),
        _ => Err(invalid_params("params must be an object")),
    }
}

/// A failed agent daemon call, for the UI: its errors pass through unchanged; a lost
/// connection is `timeout`, as while it can't be reached.
pub(crate) fn agentd_error(e: CallError) -> RpcError {
    match e {
        CallError::Rpc(e) => e,
        CallError::Timeout(after) => RpcError::new(
            ErrorData::Timeout,
            format!("the agent daemon didn't answer within {after:?}"),
        ),
        CallError::Closed(_) => agentd_unavailable(),
        CallError::Encode(e) => internal(e),
    }
}

/// Forwards `params` (already validated against the method's type) to the agent daemon.
pub(crate) async fn pass(conn: &Arc<Conn>, method: &str, params: Value) -> Result<Reply, RpcError> {
    let entry = entry(method)?;
    let mut params = object(params)?;
    fill_params(conn, entry.fill, &mut params)?;
    if entry.machines {
        conn.hub.outbound.barrier().await;
    }
    let value = conn
        .hub
        .agentd()?
        .call_raw(method, Value::Object(params))
        .await
        .map_err(agentd_error)?;
    Ok(Reply { value, after: None })
}

/// Fills `params` per `fill`.
fn fill_params(conn: &Conn, fill: Fill, params: &mut Map<String, Value>) -> Result<(), RpcError> {
    fn encode<T: serde::Serialize>(v: &T) -> Result<Value, RpcError> {
        serde_json::to_value(v).map_err(|e| internal(e.to_string()))
    }
    match fill {
        Fill::None => {}
        Fill::PromptOrigin => {
            let origin = PromptOrigin::User {
                device: conn.device_ref(),
            };
            params.insert("origin".into(), encode(&origin)?);
        }
        Fill::Responder => match conn.device_ref() {
            Some(device) => {
                params.insert("by".into(), encode(&device)?);
            }
            None => {
                params.remove("by");
            }
        },
        Fill::Sandbox => {
            let session: SessionId = params
                .get("id")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .ok_or_else(|| invalid_params("id is required"))?;
            // Without a cached summary (never seen), the agent daemon keeps the current defaults.
            if let Some(session) = conn.hub.session(session) {
                let machines: Vec<_> = session.workspaces.iter().map(|w| w.machine_id).collect();
                let linked = conn
                    .hub
                    .repo()
                    .tasks()
                    .linked_to(session.id)
                    .map_err(store_error)?;
                let spec = sandbox::defaults(&conn.hub, &machines, &linked)?;
                params.insert("sandbox".into(), encode(&spec)?);
            }
        }
    }
    Ok(())
}
