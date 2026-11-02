//! Building the protocol's typed errors.

use controlroom_client_core::daemon::CallError;
use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::MachineId;
use controlroom_protocol::ui::Connectivity;
use controlroom_protocol::{ErrorData, RpcError};

use crate::store::StoreError;

/// An unexpected failure on the controller's side.
pub(crate) fn internal(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorData::InternalError, message)
}

/// Params that parse but make no sense.
pub(crate) fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorData::InvalidParams, message)
}

/// A referenced controller object doesn't exist.
pub(crate) fn not_found(resource: Resource, id: impl ToString) -> RpcError {
    let id = id.to_string();
    let message = format!("no {resource:?} {id}").to_lowercase();
    RpcError::new(ErrorData::NotFound { resource, id }, message)
}

/// A store failure: conflicts are the caller's mistake, anything else is internal.
pub(crate) fn store_error(e: StoreError) -> RpcError {
    match e {
        StoreError::Conflict(message) => invalid_params(message),
        other => {
            tracing::error!(error = %other, "client store");
            internal(other.to_string())
        }
    }
}

/// The machine's daemon can't be reached.
pub(crate) fn machine_unavailable(machine_id: MachineId, connectivity: Connectivity) -> RpcError {
    RpcError::new(
        ErrorData::MachineUnavailable {
            machine_id,
            connectivity: Box::new(connectivity),
        },
        format!("machine {machine_id} is not connected"),
    )
}

/// A failed daemon call, for the UI: daemon errors pass through unchanged; a lost connection
/// is `machine_unavailable` with the machine's current connectivity.
pub(crate) fn call_error(
    e: CallError,
    machine_id: MachineId,
    connectivity: impl FnOnce() -> Connectivity,
) -> RpcError {
    match e {
        CallError::Rpc(e) => e,
        CallError::Timeout(after) => RpcError::new(
            ErrorData::Timeout,
            format!("the daemon didn't answer within {after:?}"),
        ),
        CallError::Closed(_) => machine_unavailable(machine_id, connectivity()),
        CallError::Encode(e) => internal(e),
    }
}
