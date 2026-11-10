//! Contact grants, devices, notifications and focus.

use controlroom_protocol::error::Resource;
use controlroom_protocol::ui::{
    Device, DeviceRefParams, DevicesRegisterParams, DevicesRenameParams, GrantsRevokeParams,
    NotificationPreferences, NotificationsSetPreferencesParams, NotificationsSubscribeParams,
};
use controlroom_protocol::{Ack, RpcError};

use super::Conn;
use crate::errors::{invalid_params, not_found, store_error};
use crate::hub::Focus;
use crate::notify::prefs;

/// Longest device label.
const MAX_LABEL_CHARS: usize = 80;

fn label(label: &str) -> Result<String, RpcError> {
    let label = label.trim();
    if label.is_empty() {
        return Err(invalid_params("the device label is empty"));
    }
    Ok(label.chars().take(MAX_LABEL_CHARS).collect())
}

fn device(conn: &Conn, id: &controlroom_protocol::ids::DeviceId) -> Result<Device, RpcError> {
    conn.hub
        .repo()
        .devices()
        .get(id)
        .map_err(store_error)?
        .ok_or_else(|| not_found(Resource::Device, id))
}

pub(super) fn grants_revoke(conn: &Conn, p: GrantsRevokeParams) -> Result<Ack, RpcError> {
    let mut st = conn.hub.lock();
    if !conn
        .hub
        .repo()
        .grants()
        .delete(p.grant_id)
        .map_err(store_error)?
    {
        return Err(not_found(Resource::Grant, p.grant_id));
    }
    st.emit_grant_removed(p.grant_id);
    Ok(Ack {})
}

/// `devices.register`: idempotent; a known device keeps the server's label. Binds the
/// connection to the device.
pub(super) fn devices_register(conn: &Conn, p: DevicesRegisterParams) -> Result<Device, RpcError> {
    let label = label(&p.label)?;
    let device = {
        let mut st = conn.hub.lock();
        let device = conn
            .hub
            .repo()
            .devices()
            .register(&p.device_id, &label)
            .map_err(store_error)?;
        st.emit_device(device.clone());
        device
    };
    if let Some(hash) = &conn.token_hash
        && let Err(e) = conn
            .hub
            .repo()
            .auth_sessions()
            .set_device(hash, &p.device_id)
    {
        tracing::warn!(error = %e, "recording the login session's device");
    }
    conn.set_device(p.device_id);
    Ok(device)
}

pub(super) fn devices_rename(conn: &Conn, p: DevicesRenameParams) -> Result<Device, RpcError> {
    let label = label(&p.label)?;
    let mut st = conn.hub.lock();
    if !conn
        .hub
        .repo()
        .devices()
        .rename(&p.device_id, &label)
        .map_err(store_error)?
    {
        return Err(not_found(Resource::Device, &p.device_id));
    }
    let device = device(conn, &p.device_id)?;
    st.emit_device(device.clone());
    Ok(device)
}

/// `devices.remove`: signs the device out everywhere: its login sessions are revoked, its open
/// connections closed and its push subscription dropped.
pub(super) fn devices_remove(conn: &Conn, p: DeviceRefParams) -> Result<Ack, RpcError> {
    crate::controller::remove_device(&conn.hub, &p.device_id)?;
    Ok(Ack {})
}

pub(super) fn preferences(conn: &Conn) -> Result<NotificationPreferences, RpcError> {
    prefs::load(&conn.hub.repo().prefs()).map_err(store_error)
}

pub(super) fn set_preferences(
    conn: &Conn,
    p: NotificationsSetPreferencesParams,
) -> Result<NotificationPreferences, RpcError> {
    prefs::check(&p).map_err(invalid_params)?;
    prefs::set(&conn.hub.repo().prefs(), &p).map_err(store_error)?;
    preferences(conn)
}

/// `notifications.subscribe`: attaches a browser's `PushSubscription.toJSON()` to a device.
pub(super) fn push_subscribe(
    conn: &Conn,
    p: NotificationsSubscribeParams,
) -> Result<Device, RpcError> {
    let row = crate::notify::parse_subscription(&p.device_id, &p.subscription)?;
    let mut st = conn.hub.lock();
    if !conn.hub.repo().push().set(&row).map_err(store_error)? {
        return Err(not_found(Resource::Device, &p.device_id));
    }
    let device = device(conn, &p.device_id)?;
    st.emit_device(device.clone());
    Ok(device)
}

pub(super) fn push_unsubscribe(conn: &Conn, p: DeviceRefParams) -> Result<Device, RpcError> {
    let mut st = conn.hub.lock();
    conn.hub
        .repo()
        .push()
        .delete(&p.device_id)
        .map_err(store_error)?;
    let device = device(conn, &p.device_id)?;
    st.emit_device(device.clone());
    Ok(device)
}

/// `client.focus`: what this connection shows, for push deduplication and presence. Closing
/// the connection clears it.
pub(super) fn focus(conn: &Conn, focus: Focus) -> Ack {
    conn.hub.set_focus(conn.client, focus);
    Ack {}
}

/// `client.activity`: the user just interacted with this connection's page.
pub(super) fn activity(conn: &Conn) -> Ack {
    conn.hub.record_input(conn.client);
    Ack {}
}
