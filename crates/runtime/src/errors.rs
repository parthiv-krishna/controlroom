//! Constructors for the typed errors the runtime returns.

use controlroom_protocol::ids::SessionId;
use controlroom_protocol::session::SessionState;
use controlroom_protocol::{ErrorData, Resource, RpcError};

pub(crate) fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorData::InternalError, e.to_string())
}

pub(crate) fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorData::InvalidParams, message)
}

pub(crate) fn invalid_state(state: SessionState, what: &str) -> RpcError {
    RpcError::new(
        ErrorData::InvalidState { state },
        format!("{what} doesn't apply to a session in state {state:?}"),
    )
}

pub(crate) fn unknown_session(id: SessionId) -> RpcError {
    RpcError::new(ErrorData::UnknownSession { id }, format!("no session {id}"))
}

pub(crate) fn not_found(resource: Resource, id: impl ToString) -> RpcError {
    let id = id.to_string();
    RpcError::new(
        ErrorData::NotFound {
            resource,
            id: id.clone(),
        },
        format!("no {resource:?} {id}"),
    )
}

pub(crate) fn busy(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorData::Busy, message)
}

pub(crate) fn unsupported(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorData::Unsupported, message)
}
