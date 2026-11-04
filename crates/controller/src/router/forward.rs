//! One forwarded host request: the call on the host connection and its answer.

use std::sync::Weak;

use controlroom_client_core::daemon::{CallError, CallOptions, CallTimeout, DaemonClient};
use controlroom_protocol::host::methods::{DoctorReportMethod, FsList, ToolAttach, ToolRun};
use controlroom_protocol::ids::HostOpId;
use controlroom_protocol::{ErrorData, Method, RpcError, RpcOutcome};
use serde_json::Value;

use crate::errors::internal;
use crate::hub::Hub;

/// How long a host method may take: tool calls as long as their command (the host enforces
/// its timeout, the agent daemon the grace), slow checks the slow timeout.
fn timeout(hub: &Hub, method: &str) -> CallTimeout {
    if method == ToolRun::NAME || method == ToolAttach::NAME {
        CallTimeout::Never
    } else if method == FsList::NAME || method == DoctorReportMethod::NAME {
        CallTimeout::After(hub.services.tuning.slow_call_timeout)
    } else {
        CallTimeout::Default
    }
}

/// Calls the host and hands the answer to the router; a closed connection is no answer.
pub(super) async fn run(
    hub: Weak<Hub>,
    client: DaemonClient,
    id: HostOpId,
    flight: u64,
    method: &'static str,
    params: Value,
) {
    let options = match hub.upgrade() {
        Some(hub) => CallOptions {
            timeout: timeout(&hub, method),
            ..CallOptions::default()
        },
        None => return,
    };
    let outcome = match client.call_raw_with(method, params, options).await {
        Ok(result) => Some(RpcOutcome::Result(result)),
        Err(CallError::Rpc(e)) => Some(RpcOutcome::Error(e)),
        Err(CallError::Timeout(after)) => Some(RpcOutcome::Error(RpcError::new(
            ErrorData::Timeout,
            format!("the host didn't answer {method} within {after:?}"),
        ))),
        Err(CallError::Encode(e)) => Some(RpcOutcome::Error(internal(e))),
        Err(CallError::Closed(_)) => None,
    };
    let Some(hub) = hub.upgrade() else { return };
    if let Some(answer) = hub.hostops_finished(&id, flight, outcome) {
        hub.outbound.respond(answer).await;
    }
}
