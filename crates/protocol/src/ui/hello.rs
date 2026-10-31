//! The UI `hello`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::DeviceId;
use crate::version::{Capability, ProtocolVersion};

/// Params of the UI `hello`, the first call on every WebSocket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct UiHelloParams {
    /// The UI protocol version the client speaks.
    pub protocol_version: ProtocolVersion,
    /// A label for logs, e.g. the user agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    /// This browser's device id, binding the connection to the device for attribution and
    /// revocation. The client registers it with `devices.register` right after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
}

/// How the server authenticates users.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// OIDC login.
    Oidc,
    /// The development token (loopback only).
    DevToken,
    /// A mode this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// The signed-in user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct UserInfo {
    /// The owner id (the OIDC subject).
    pub id: String,
    /// A display name, if the identity provider supplied one.
    pub name: Option<String>,
    /// An email address, if the identity provider supplied one.
    pub email: Option<String>,
}

/// Web Push support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PushInfo {
    /// The VAPID public key (base64url) for `PushManager.subscribe`.
    pub vapid_public_key: String,
}

/// Result of the UI `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct UiHelloResult {
    /// The server's software version (also the daemon version it deploys).
    pub server_version: String,
    /// The UI protocol version the server speaks.
    pub protocol_version: ProtocolVersion,
    /// How users authenticate.
    pub auth: AuthMode,
    /// The signed-in user.
    pub user: UserInfo,
    /// Web Push support; `null` when push isn't configured.
    pub push: Option<PushInfo>,
    /// Optional server features (e.g. `schedules`); clients ignore unknown ones.
    pub capabilities: Vec<Capability>,
}
