//! Routes one request (after `hello`). Sessions moved to the agent daemon, so this daemon
//! serves only `agents.list`, `fs.list` and `doctor.report` (on blocking threads), plus an
//! empty `machine.subscribe` snapshot and default `settings.get` so the controller still
//! connects, and answers every other method with `unsupported`.

use std::sync::Arc;

use controlroom_protocol::daemon::{
    AgentsListResult, DaemonRequest, DaemonSettings, MachineSnapshot, MachineSubscribeResult,
    RetentionPolicy,
};
use controlroom_protocol::ids::{Rev, SubscriptionId};
use controlroom_protocol::{ErrorData, Resource, RpcError, RpcRequest};
use serde::Serialize;
use serde_json::Value;

use super::conn::Conn;
use super::fs_list;
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

/// Runs blocking work off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, RpcError> + Send + 'static,
) -> Result<T, RpcError> {
    tokio::task::spawn_blocking(f).await.map_err(internal)?
}

/// The settings this daemon reports: the defaults (they applied to sessions, which are gone).
fn settings() -> DaemonSettings {
    DaemonSettings {
        idle_timeout_h: 48,
        retention: RetentionPolicy {
            delete_closed_after_days: None,
        },
    }
}

/// Handles `request` and queues its response.
pub async fn handle(conn: &Arc<Conn>, request: RpcRequest<Value>) {
    let id = request.id.clone();
    let call = match DaemonRequest::from_request(request) {
        Ok(call) => call,
        Err(e) => return conn.respond(id, Err(e)).await,
    };
    let method = call.method();
    let result = match call {
        DaemonRequest::Hello(_) => Err(RpcError::new(
            ErrorData::InvalidRequest,
            "hello was already called on this connection",
        )),
        DaemonRequest::MachineSubscribe(_) => json(Ok(MachineSubscribeResult {
            subscription_id: SubscriptionId::generate(),
            rev: Rev(0),
            snapshot: Some(MachineSnapshot {
                sessions: Vec::new(),
                settings: settings(),
            }),
        })),
        DaemonRequest::SettingsGet(_) => json(Ok(settings())),
        DaemonRequest::Unsubscribe(p) => Err(RpcError::new(
            ErrorData::NotFound {
                resource: Resource::Subscription,
                id: p.subscription_id.to_string(),
            },
            "no such subscription on this connection",
        )),
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
        _ => Err(RpcError::new(
            ErrorData::Unsupported,
            format!("{method}: sessions moved to controlroom-agentd"),
        )),
    };
    conn.respond(id, result).await;
}
