//! Table-driven pass-through of session-scoped and machine-scoped daemon methods.
//!
//! The UI params are the daemon params plus `machine_id`. The controller strips `machine_id`,
//! fills in what only it knows (per [`Fill`]), and forwards the call unchanged otherwise:
//! `op_id`s pass through as they are, and daemon results and errors come back unchanged. A
//! machine that isn't connected answers `machine_unavailable`; nothing is queued.

use std::sync::Arc;

use controlroom_client_core::daemon::CallOptions;
use controlroom_protocol::RpcError;
use controlroom_protocol::daemon::{DaemonSettings, DoctorReport};
use controlroom_protocol::ids::{MachineId, SessionId};
use controlroom_protocol::session::PromptOrigin;
use serde_json::{Map, Value};

use super::{Conn, Reply, sandbox};
use crate::errors::{call_error, internal, invalid_params};

/// What the controller fills in before forwarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fill {
    /// Nothing.
    None,
    /// `origin`: the user on this connection's device (replacing any value sent).
    PromptOrigin,
    /// `sandbox`: the session's refreshed defaults (machine and matching projects).
    Sandbox,
    /// `by`: the deciding device is this connection's, always (a client's value is dropped).
    Responder,
}

/// What the controller does with the result.
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

/// Every passed-through method.
const TABLE: &[Entry] = &[
    Entry {
        fill: Fill::PromptOrigin,
        ..plain("session.prompt")
    },
    plain("session.cancel"),
    plain("session.close"),
    plain("session.reopen"),
    Entry {
        fill: Fill::Sandbox,
        ..plain("session.resume")
    },
    Entry {
        fill: Fill::Sandbox,
        ..plain("session.start_fresh")
    },
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
    Entry {
        fill: Fill::Responder,
        ..plain("request.respond")
    },
    plain("todo.add"),
    plain("todo.update"),
    plain("todo.remove"),
    plain("todo.reorder"),
    plain("job.list"),
    plain("job.logs"),
    plain("job.kill"),
    plain("job.follow"),
    plain("job.unfollow"),
    plain("sandbox.revoke"),
    plain("agents.list"),
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

/// Whether `method` is passed through.
pub(crate) fn is_passed_through(method: &str) -> bool {
    TABLE.iter().any(|e| e.method == method)
}

/// Forwards `params` (already validated against the method's type) to the machine's daemon.
pub(crate) async fn pass(conn: &Arc<Conn>, method: &str, params: Value) -> Result<Reply, RpcError> {
    let entry = TABLE
        .iter()
        .find(|e| e.method == method)
        .ok_or_else(|| internal(format!("{method} is not passed through")))?;
    let hub = &conn.hub;
    let Value::Object(mut params) = params else {
        return Err(invalid_params("params must be an object"));
    };
    let machine_id: MachineId = params
        .remove("machine_id")
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| invalid_params("machine_id is required"))?;
    fill(conn, entry.fill, machine_id, &mut params)?;
    let daemon = hub.daemon(machine_id)?;
    let tuning = &hub.services.tuning;
    let options = CallOptions {
        timeout: entry.slow.then_some(tuning.slow_call_timeout),
        ..CallOptions::default()
    };
    let value = daemon
        .call_raw_with(method, Value::Object(params), options)
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

fn fill(
    conn: &Conn,
    fill: Fill,
    machine_id: MachineId,
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
            if let Some(summary) = conn.hub.session(machine_id, session) {
                let linked = conn
                    .hub
                    .repo()
                    .tasks()
                    .linked_to(machine_id, session)
                    .map_err(crate::errors::store_error)?;
                let spec = sandbox::defaults(&conn.hub, machine_id, &summary.cwd, &linked)?;
                params.insert("sandbox".into(), encode(&spec)?);
            }
        }
    }
    Ok(())
}
