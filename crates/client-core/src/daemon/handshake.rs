//! The controller's `hello`: protocol version, the controller role and the
//! typed refusals a daemon can answer with.

use controlroom_protocol::daemon::methods::Hello;
use controlroom_protocol::daemon::{ClientRole, HelloParams, HelloResult};
use controlroom_protocol::ids::ControllerId;
use controlroom_protocol::{DAEMON_PROTOCOL_VERSION, ErrorData, ProtocolVersion, UnixMillis};

use super::{CallError, DaemonClient};

/// Why the handshake failed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum HelloError {
    /// The daemon speaks another protocol major: the machine is `incompatible`
    /// until it is upgraded.
    #[error(
        "controlroomd {daemon_version} speaks protocol {protocol_version}, not {DAEMON_PROTOCOL_VERSION}"
    )]
    Incompatible {
        /// The daemon's software version.
        daemon_version: String,
        /// The daemon's protocol version.
        protocol_version: ProtocolVersion,
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

/// Says `hello` as controller `controller_id`. A daemon answering with a
/// compatible-looking error but an incompatible version is reported as
/// [`HelloError::Incompatible`] too.
pub async fn hello(
    client: &DaemonClient,
    client_name: &str,
    controller_id: ControllerId,
) -> Result<HelloResult, HelloError> {
    let params = HelloParams {
        protocol_version: DAEMON_PROTOCOL_VERSION,
        client_name: client_name.to_owned(),
        role: ClientRole::Controller { controller_id },
    };
    match client.call::<Hello>(&params).await {
        Ok(result) if !DAEMON_PROTOCOL_VERSION.is_compatible_with(result.protocol_version) => {
            Err(HelloError::Incompatible {
                daemon_version: result.daemon_version,
                protocol_version: result.protocol_version,
            })
        }
        Ok(result) => Ok(result),
        Err(CallError::Rpc(e)) => match e.data {
            Some(ErrorData::IncompatibleVersion {
                protocol_version,
                version,
            }) => Err(HelloError::Incompatible {
                daemon_version: version,
                protocol_version,
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
