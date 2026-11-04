//! Devices, push notifications and contact grants.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::ids::{DeviceId, GrantId, SessionId, UnixMillis};
use crate::session::DeviceRef;

/// A browser the user signed in from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Device {
    /// The device.
    pub device_id: DeviceId,
    /// User-chosen label (default: a guess such as "Firefox on Linux").
    pub label: String,
    /// When it was registered.
    pub created_at: UnixMillis,
    /// When it last connected.
    pub last_seen: UnixMillis,
    /// Whether it has a push subscription.
    pub push: bool,
}

/// Params of `devices.register`, sent after `hello`. Idempotent: an existing device keeps its
/// server-side label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DevicesRegisterParams {
    /// The device.
    pub device_id: DeviceId,
    /// The label to use if the device is new.
    pub label: String,
}

/// Params of `devices.rename`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DevicesRenameParams {
    /// The device.
    pub device_id: DeviceId,
    /// The new label.
    pub label: String,
}

/// Params of `devices.remove`: revokes the device's login sessions, closes its WebSockets and
/// drops its push subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DeviceRefParams {
    /// The device.
    pub device_id: DeviceId,
}

/// Which events push to devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NotificationPreferences {
    /// Blocking approvals, failures and interruptions.
    pub attention: bool,
    /// Non-blocking questions (in-app only by default).
    pub questions: bool,
    /// Finished runs with unread output.
    pub turn_end: bool,
}

/// Params of `notifications.set_preferences`. Absent fields are left unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NotificationsSetPreferencesParams {
    /// Push on attention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<bool>,
    /// Push on questions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<bool>,
    /// Push on finished runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_end: Option<bool>,
}

/// Params of `notifications.subscribe`: attach a Web Push subscription to a device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct NotificationsSubscribeParams {
    /// The device.
    pub device_id: DeviceId,
    /// The browser's `PushSubscription.toJSON()`.
    #[ts(type = "unknown")]
    pub subscription: Value,
}

/// How long a contact grant lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ContactScope {
    /// One operation.
    Once,
    /// The pair of sessions, until revoked.
    Pair,
    /// A scope this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// A user-approved permission for two sessions to contact each other outside the default
/// policy (same task, parent and child).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ContactGrant {
    /// The grant.
    pub id: GrantId,
    /// The requesting session.
    pub from: SessionId,
    /// The contacted session.
    pub to: SessionId,
    /// How long it lasts.
    pub scope: ContactScope,
    /// When it was granted.
    pub granted_at: UnixMillis,
    /// The granting device.
    pub by: Option<DeviceRef>,
}

/// Params of `grants.revoke`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct GrantsRevokeParams {
    /// The grant.
    pub grant_id: GrantId,
}
