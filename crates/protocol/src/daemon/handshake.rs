//! The daemon `hello`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{ControllerId, DaemonId};
use crate::version::{Capability, ProtocolVersion};

/// Params of the daemon `hello`, the first call on every connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HelloParams {
    /// The protocol version the client speaks.
    pub protocol_version: ProtocolVersion,
    /// A short label for logs, e.g. `"controlroom-web 0.1.0"`.
    pub client_name: String,
    /// The calling controller.
    pub controller_id: ControllerId,
    /// Claim the mailbox broker role for this controller. The latest claim wins; the previous
    /// holder's outbox subscription ends.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub claim_broker: bool,
}

/// Result of the daemon `hello`. A major mismatch is answered with an `incompatible_version`
/// error instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HelloResult {
    /// The `controlroomd` software version, e.g. `"0.1.0"`.
    pub daemon_version: String,
    /// The protocol version the daemon speaks (same major as the client's).
    pub protocol_version: ProtocolVersion,
    /// The daemon installation's stable id.
    pub daemon_id: DaemonId,
    /// Optional features this daemon implements; clients ignore unknown ones.
    pub capabilities: Vec<Capability>,
    /// The controller holding the mailbox broker claim, after this call.
    pub broker: Option<ControllerId>,
}
