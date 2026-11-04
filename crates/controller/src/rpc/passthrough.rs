//! Table-driven pass-through of session-scoped daemon methods.
//!
//! - **Old UI API**: the params are a legacy daemon's params plus `machine_id`; the controller
//!   strips `machine_id` and forwards to that machine. A machine that isn't connected answers
//!   `machine_unavailable`; nothing is queued.
//! - **Next UI API**: the params are the agent daemon's, unchanged, and go to the agent daemon.
//!   While it can't be reached the call fails with `timeout`.
//!
//! Either way the controller fills in what only it knows (per [`Fill`]) and forwards the call
//! unchanged otherwise: `op_id`s pass through as they are, and results and errors come back
//! unchanged.

use std::sync::Arc;

use controlroom_client_core::daemon::{CallError, CallOptions, CallTimeout};
use controlroom_protocol::daemon::{DaemonSettings, DoctorReport};
use controlroom_protocol::ids::{MachineId, SessionId};
use controlroom_protocol::session::PromptOrigin;
use controlroom_protocol::{ErrorData, RpcError};
use serde_json::{Map, Value};

use super::{Conn, Reply, sandbox};
use crate::errors::{call_error, internal, invalid_params, store_error};
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

/// What the controller does with a legacy daemon's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keep {
    /// Nothing.
    None,
    /// Stores the doctor report on the machine.
    Doctor,
    /// Stores the daemon settings on the machine.
    Settings,
}

/// One passed-through method.
struct Entry {
    method: &'static str,
    fill: Fill,
    keep: Keep,
    /// Uses the slow-call timeout.
    slow: bool,
}

const fn plain(method: &'static str) -> Entry {
    Entry {
        method,
        fill: Fill::None,
        keep: Keep::None,
        slow: false,
    }
}

const fn fill(method: &'static str, fill: Fill) -> Entry {
    Entry {
        fill,
        ..plain(method)
    }
}

/// The session-scoped methods both APIs pass through.
macro_rules! session_methods {
    ($($extra:expr),* $(,)?) => {
        &[
            fill("session.prompt", Fill::PromptOrigin),
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
            $($extra,)*
        ]
    };
}

/// The old UI API's table (to a legacy daemon).
const LEGACY: &[Entry] = session_methods![
    Entry {
        slow: true,
        ..plain("fs.list")
    },
    Entry {
        keep: Keep::Doctor,
        slow: true,
        ..plain("doctor.report")
    },
    Entry {
        keep: Keep::Settings,
        ..plain("settings.get")
    },
    Entry {
        keep: Keep::Settings,
        ..plain("settings.set")
    },
];

/// The next UI API's table (to the agent daemon).
const NEXT: &[Entry] = session_methods![
    plain("session.workspace_add"),
    plain("session.workspace_remove"),
    plain("settings.get"),
    plain("settings.set"),
];

/// Whether the old UI API passes `method` through.
pub(crate) fn is_passed_through(method: &str) -> bool {
    LEGACY.iter().any(|e| e.method == method)
}

/// Whether the next UI API passes `method` through to the agent daemon.
pub(crate) fn is_passed_to_agentd(method: &str) -> bool {
    NEXT.iter().any(|e| e.method == method)
}

fn entry(table: &'static [Entry], method: &str) -> Result<&'static Entry, RpcError> {
    table
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

fn options(conn: &Conn, entry: &Entry) -> CallOptions {
    CallOptions {
        timeout: if entry.slow {
            CallTimeout::After(conn.hub.services.tuning.slow_call_timeout)
        } else {
            CallTimeout::Default
        },
        ..CallOptions::default()
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

/// Old UI API: forwards `params` (already validated against the method's type) to the
/// machine's legacy daemon.
pub(crate) async fn pass(conn: &Arc<Conn>, method: &str, params: Value) -> Result<Reply, RpcError> {
    let entry = entry(LEGACY, method)?;
    let hub = &conn.hub;
    let mut params = object(params)?;
    let machine_id: MachineId = params
        .remove("machine_id")
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| invalid_params("machine_id is required"))?;
    fill_params(conn, entry.fill, Some(machine_id), &mut params)?;
    let daemon = hub.daemon(machine_id)?;
    let value = daemon
        .call_raw_with(method, Value::Object(params), options(conn, entry))
        .await
        .map_err(|e| call_error(e, machine_id, || hub.connectivity(machine_id)))?;
    match entry.keep {
        Keep::None => {}
        Keep::Doctor => {
            if let Ok(report) = serde_json::from_value::<DoctorReport>(value.clone()) {
                hub.set_doctor(machine_id, &report);
            }
        }
        Keep::Settings => {
            if let Ok(settings) = serde_json::from_value::<DaemonSettings>(value.clone()) {
                hub.set_daemon_settings(machine_id, &settings);
            }
        }
    }
    Ok(Reply { value, after: None })
}

/// Next UI API: forwards `params` (already validated) to the agent daemon.
pub(crate) async fn pass_to_agentd(
    conn: &Arc<Conn>,
    method: &str,
    params: Value,
) -> Result<Reply, RpcError> {
    let entry = entry(NEXT, method)?;
    let mut params = object(params)?;
    fill_params(conn, entry.fill, None, &mut params)?;
    let value = conn
        .hub
        .agentd()?
        .call_raw_with(method, Value::Object(params), options(conn, entry))
        .await
        .map_err(agentd_error)?;
    Ok(Reply { value, after: None })
}

/// Fills `params`; `machine` is the legacy daemon's machine (old UI API) or `None` (agent
/// daemon).
fn fill_params(
    conn: &Conn,
    fill: Fill,
    machine: Option<MachineId>,
    params: &mut Map<String, Value>,
) -> Result<(), RpcError> {
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
            // Without a cached summary (never seen), the daemon keeps the current defaults.
            let machines = match machine {
                Some(machine) => conn.hub.session(machine, session).map(|_| vec![machine]),
                None => conn
                    .hub
                    .global_session(session)
                    .map(|s| s.workspaces.iter().map(|w| w.machine_id).collect()),
            };
            if let Some(machines) = machines {
                let linked = conn
                    .hub
                    .repo()
                    .tasks()
                    .linked_to(session)
                    .map_err(store_error)?;
                let spec = sandbox::defaults(&conn.hub, &machines, &linked)?;
                params.insert("sandbox".into(), encode(&spec)?);
            }
        }
    }
    Ok(())
}
