//! The controller's `hello`: protocol version, the controller role and the
//! typed refusals a daemon can answer with.
//!
//! The daemon API, the agent daemon API and the host API share the `hello`
//! params and refusals; they differ in the protocol version and the result.
//! [`hello`] speaks the daemon API, [`agentd_hello`] the agent daemon API and
//! [`host_hello`] the host API. [`hello_value`] returns the raw result, for a
//! caller that must tell daemon generations apart by its shape.

use controlroom_protocol::daemon::methods::Hello;
use controlroom_protocol::daemon::{ClientRole, HelloParams, HelloResult};
use controlroom_protocol::host::HostHelloResult;
use controlroom_protocol::ids::ControllerId;
use controlroom_protocol::{
    AGENTD_PROTOCOL_VERSION, DAEMON_PROTOCOL_VERSION, ErrorData, HOST_PROTOCOL_VERSION, Method,
    ProtocolVersion, UnixMillis,
};
use serde_json::Value;

use super::{CallError, DaemonClient};

/// Why the handshake failed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum HelloError {
    /// The daemon speaks another protocol major: the machine is `incompatible`
    /// until it is upgraded.
    #[error("{daemon_version} speaks protocol {protocol_version}, not {expected}")]
    Incompatible {
        /// The daemon's software version.
        daemon_version: String,
        /// The daemon's protocol version.
        protocol_version: ProtocolVersion,
        /// The protocol version this client speaks.
        expected: ProtocolVersion,
    },
    /// Another controller is connected to this daemon (one controller per daemon).
    #[error("another controller ({controller_id}) is connected to this daemon")]
    ControllerConflict {
        /// The connected controller.
        controller_id: ControllerId,
        /// Since when.
        connected_since: UnixMillis,
    },
    /// The call itself failed.
    #[error("daemon hello failed: {0}")]
    Call(CallError),
}

/// Says the daemon API `hello` as controller `controller_id`. A daemon
/// answering with an incompatible version is reported as
/// [`HelloError::Incompatible`].
pub async fn hello(
    client: &DaemonClient,
    client_name: &str,
    controller_id: ControllerId,
) -> Result<HelloResult, HelloError> {
    let result: HelloResult =
        typed(hello_value(client, DAEMON_PROTOCOL_VERSION, client_name, controller_id).await?)?;
    check_version(
        DAEMON_PROTOCOL_VERSION,
        &result.daemon_version,
        result.protocol_version,
    )?;
    Ok(result)
}

/// Says the agent daemon API `hello` as controller `controller_id`.
pub async fn agentd_hello(
    client: &DaemonClient,
    client_name: &str,
    controller_id: ControllerId,
) -> Result<HelloResult, HelloError> {
    let result: HelloResult =
        typed(hello_value(client, AGENTD_PROTOCOL_VERSION, client_name, controller_id).await?)?;
    check_version(
        AGENTD_PROTOCOL_VERSION,
        &result.daemon_version,
        result.protocol_version,
    )?;
    Ok(result)
}

/// Says the host API `hello` as controller `controller_id`.
pub async fn host_hello(
    client: &DaemonClient,
    client_name: &str,
    controller_id: ControllerId,
) -> Result<HostHelloResult, HelloError> {
    let result: HostHelloResult =
        typed(hello_value(client, HOST_PROTOCOL_VERSION, client_name, controller_id).await?)?;
    check_version(
        HOST_PROTOCOL_VERSION,
        &result.host_version,
        result.protocol_version,
    )?;
    Ok(result)
}

/// Says `hello` speaking `version` as controller `controller_id` and returns the raw result.
/// Typed refusals (incompatible version, another controller) are mapped; the result's version
/// is not checked.
pub async fn hello_value(
    client: &DaemonClient,
    version: ProtocolVersion,
    client_name: &str,
    controller_id: ControllerId,
) -> Result<Value, HelloError> {
    let params = HelloParams {
        protocol_version: version,
        client_name: client_name.to_owned(),
        role: ClientRole::Controller { controller_id },
    };
    let params = serde_json::to_value(&params)
        .map_err(|e| HelloError::Call(CallError::Encode(e.to_string())))?;
    match client.call_raw(Hello::NAME, params).await {
        Ok(result) => Ok(result),
        Err(CallError::Rpc(e)) => match e.data {
            Some(ErrorData::IncompatibleVersion {
                protocol_version,
                version: daemon_version,
            }) => Err(HelloError::Incompatible {
                daemon_version,
                protocol_version,
                expected: version,
            }),
            Some(ErrorData::ControllerConflict {
                controller_id,
                connected_since,
            }) => Err(HelloError::ControllerConflict {
                controller_id,
                connected_since,
            }),
            _ => Err(HelloError::Call(CallError::Rpc(e))),
        },
        Err(e) => Err(HelloError::Call(e)),
    }
}

/// Decodes a `hello` result; a mismatch fails as `invalid_response`.
fn typed<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, HelloError> {
    serde_json::from_value(value).map_err(|e| {
        HelloError::Call(CallError::Rpc(controlroom_protocol::RpcError::new(
            ErrorData::InvalidResponse,
            format!("malformed hello result: {e}"),
        )))
    })
}

/// A compatible-looking answer with another major is incompatible too.
pub fn check_version(
    expected: ProtocolVersion,
    daemon_version: &str,
    protocol_version: ProtocolVersion,
) -> Result<(), HelloError> {
    if expected.is_compatible_with(protocol_version) {
        Ok(())
    } else {
        Err(HelloError::Incompatible {
            daemon_version: daemon_version.to_owned(),
            protocol_version,
            expected,
        })
    }
}
