//! The daemon `hello` and the single-controller rule.
//!
//! A daemon serves exactly one controller at a time. A `hello` from a controller with a
//! different `controller_id` while one is connected fails with `controller_conflict`. A `hello`
//! with the *same* `controller_id` replaces the old connection, which the daemon closes (ending
//! its subscriptions), so stale or half-open connections never block a reconnect.
//!
//! Debug clients (`controlroomd ctl`) don't count as the controller: any number of them may
//! connect alongside it, read and issue commands, but never consume the mailbox.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{ControllerId, DaemonId};
use crate::version::{Capability, ProtocolVersion};

/// What kind of client is connecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientRole {
    /// The controller: at most one per daemon, and the only client that may use `outbox.*` and
    /// `inbox.deliver`.
    Controller {
        /// The calling controller installation.
        controller_id: ControllerId,
    },
    /// A local debugging client such as `controlroomd ctl`. It may subscribe and issue
    /// commands, but mailbox methods answer it with `unsupported`.
    Debug,
}

/// Params of the daemon `hello`, the first call on every connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HelloParams {
    /// The protocol version the client speaks.
    pub protocol_version: ProtocolVersion,
    /// A short label for logs, e.g. `"controlroom-web 0.1.0"`.
    pub client_name: String,
    /// The controller (with its id) or a debug client.
    pub role: ClientRole,
}

/// Result of the daemon `hello`. A major mismatch is answered with an `incompatible_version`
/// error, and a second controller with `controller_conflict`.
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
}
