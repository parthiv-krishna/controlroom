//! Machines (`machines.*`): the add flow, host-key confirmation, removal, reconnect, upgrade and
//! the controller's machine settings.
//!
//! **Add flow:** `machines.add` fetches the host key for the user to confirm (nothing is
//! saved). `machines.add_confirm` checks the host still presents the confirmed key, connects
//! with the chosen key (a rejected key answers `permission_denied` with the install command;
//! the user installs it and calls `add_confirm` again), probes the platform, then saves the
//! machine with its host key pinned. Bootstrap (install or upgrade with verify-run and
//! rollback, start, doctor) continues in the machine's task; its progress and the doctor
//! report arrive through the state feed.

use std::sync::Arc;

use controlroom_client_core::bootstrap::{self, BootstrapError};
use controlroom_client_core::keys::install_command;
use controlroom_client_core::ssh::{self, HostKey, SshError, SshTarget};
use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::KeyId;
use controlroom_protocol::ui::{
    Machine, MachineRef, MachinesAddConfirmParams, MachinesAddConfirmResult, MachinesAddResult,
    MachinesConfirmHostKeyParams, MachinesSetSettingsParams, NewMachine,
};
use controlroom_protocol::{Ack, ErrorData, RpcError};

use super::Conn;
use crate::errors::{internal, invalid_params, not_found, store_error};
use crate::hub::host_key_info;
use crate::keys;
use crate::machines::connect::{ConnectPlan, connect_any};
use crate::machines::{InstallPolicy, MachineCommand};
use crate::store::{KeyKind, KeyRow, NewMachineRow};

fn host_key_mismatch(expected: String, actual: String) -> RpcError {
    let message = format!("the host presented {actual}, not the confirmed {expected}");
    RpcError::new(ErrorData::HostKeyMismatch { expected, actual }, message)
}

/// SSH failures while adding or re-pinning a machine.
fn ssh_error(e: SshError) -> RpcError {
    match e {
        SshError::HostKeyMismatch { expected, actual } => {
            host_key_mismatch(expected.fingerprint(), actual.fingerprint())
        }
        other => internal(other.to_string()),
    }
}

fn target(m: &NewMachine) -> Result<SshTarget, RpcError> {
    let (host, user) = (m.host.trim(), m.user.trim());
    if host.is_empty() || user.is_empty() || m.port == 0 {
        return Err(invalid_params(
            "host, user and a non-zero port are required",
        ));
    }
    Ok(SshTarget::new(host, m.port, user))
}

/// Refuses a machine that is already added (same user, host and port).
fn check_new(conn: &Conn, t: &SshTarget) -> Result<(), RpcError> {
    let existing = conn
        .hub
        .repo()
        .machines()
        .find(&t.host, t.port, &t.username)
        .map_err(store_error)?;
    match existing {
        Some(m) => Err(invalid_params(format!(
            "{}@{}:{} is already added as {:?}",
            t.username, t.host, t.port, m.name
        ))),
        None => Ok(()),
    }
}

/// The key a machine authenticates with: an additional key, or the controlroom key.
fn key_for(conn: &Conn, key_id: Option<KeyId>) -> Result<KeyRow, RpcError> {
    match key_id {
        Some(id) => conn
            .hub
            .repo()
            .keys()
            .get(id)
            .map_err(store_error)?
            .filter(|k| k.kind == KeyKind::Additional)
            .ok_or_else(|| not_found(Resource::Key, id)),
        None => keys::ensure_controlroom_key(&conn.hub.services, &conn.hub.owner),
    }
}

/// Fetches the host key, checking it against the fingerprint the user confirmed.
async fn confirmed_host_key(
    conn: &Conn,
    t: &SshTarget,
    fingerprint: &str,
) -> Result<HostKey, RpcError> {
    let host_key = ssh::fetch_host_key(t, &conn.hub.services.tuning.ssh)
        .await
        .map_err(ssh_error)?;
    if host_key.fingerprint() != fingerprint {
        return Err(host_key_mismatch(
            fingerprint.to_owned(),
            host_key.fingerprint(),
        ));
    }
    Ok(host_key)
}

pub(super) async fn add(conn: &Conn, m: NewMachine) -> Result<MachinesAddResult, RpcError> {
    let t = target(&m)?;
    check_new(conn, &t)?;
    let host_key = ssh::fetch_host_key(&t, &conn.hub.services.tuning.ssh)
        .await
        .map_err(ssh_error)?;
    Ok(MachinesAddResult {
        host_key: host_key_info(&host_key),
    })
}

pub(super) async fn add_confirm(
    conn: &Arc<Conn>,
    p: MachinesAddConfirmParams,
) -> Result<MachinesAddConfirmResult, RpcError> {
    let hub = &conn.hub;
    let t = target(&p.machine)?;
    check_new(conn, &t)?;
    let name = p
        .machine
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&t.host)
        .to_owned();
    let key = key_for(conn, p.machine.key_id)?;
    let host_key = confirmed_host_key(conn, &t, &p.host_key_fingerprint).await?;
    let plan = ConnectPlan {
        target: t.clone(),
        pinned: Some(host_key.clone()),
        keys: vec![(key.id, keys::unseal(&hub.services, &key)?)],
    };
    let (ssh, key_id) = match connect_any(&plan, &hub.services.tuning.ssh).await {
        Ok(connected) => connected,
        Err(SshError::AuthFailed { .. }) => {
            return Ok(MachinesAddConfirmResult::PermissionDenied {
                install_command: install_command(&key.public_key),
                public_key: key.public_key,
            });
        }
        Err(e) => return Err(ssh_error(e)),
    };
    // Unsupported platforms are refused before anything is saved.
    if let Err(e) = bootstrap::probe(&ssh).await {
        ssh.disconnect().await;
        return Err(match e {
            BootstrapError::UnsupportedPlatform(u) => RpcError::new(
                ErrorData::UnsupportedPlatform {
                    uname: format!("{} {}", u.os, u.machine),
                },
                u.to_string(),
            ),
            other => internal(format!("probing the machine: {other}")),
        });
    }
    let record = hub
        .repo()
        .machines()
        .insert(NewMachineRow {
            name,
            host: t.host.clone(),
            port: t.port,
            username: t.username.clone(),
            key_id: p.machine.key_id,
            auth_key_id: p.machine.key_id.is_none().then_some(key_id),
            host_key: host_key.to_openssh(),
        })
        .map_err(store_error);
    let record = match record {
        Ok(record) => record,
        Err(e) => {
            ssh.disconnect().await;
            return Err(e);
        }
    };
    let id = record.id;
    hub.spawn_machine(record, Some((ssh, key_id)), InstallPolicy::IfOlder);
    Ok(MachinesAddConfirmResult::Added {
        machine: Box::new(hub.machine_view(id)?),
    })
}

pub(super) async fn confirm_host_key(
    conn: &Conn,
    p: MachinesConfirmHostKeyParams,
) -> Result<Machine, RpcError> {
    let hub = &conn.hub;
    let record = hub
        .repo()
        .machines()
        .get(p.machine_id)
        .map_err(store_error)?
        .ok_or_else(|| not_found(Resource::Machine, p.machine_id))?;
    let t = SshTarget::new(record.host, record.port, record.username);
    let host_key = confirmed_host_key(conn, &t, &p.host_key_fingerprint).await?;
    hub.repo()
        .machines()
        .pin_host_key(p.machine_id, &host_key.to_openssh())
        .map_err(store_error)?;
    hub.reload_record(p.machine_id)?;
    hub.command(p.machine_id, MachineCommand::Reconnect)?;
    hub.machine_view(p.machine_id)
}

/// Removes the record; the machine's sessions keep running there. Task links stay.
pub(super) async fn remove(conn: &Conn, p: MachineRef) -> Result<Ack, RpcError> {
    let hub = &conn.hub;
    hub.machine_view(p.machine_id)?;
    hub.remove_machine(p.machine_id).await;
    hub.repo()
        .machines()
        .delete(p.machine_id)
        .map_err(store_error)?;
    Ok(Ack {})
}

pub(super) fn reconnect(conn: &Conn, p: MachineRef) -> Result<Ack, RpcError> {
    conn.hub.command(p.machine_id, MachineCommand::Reconnect)?;
    Ok(Ack {})
}

pub(super) fn upgrade(conn: &Conn, p: MachineRef) -> Result<Ack, RpcError> {
    conn.hub.machine_view(p.machine_id)?;
    if let Err(why) = &conn.hub.services.binaries {
        return Err(RpcError::new(
            ErrorData::Unsupported,
            format!("can't upgrade: {why}"),
        ));
    }
    conn.hub.command(p.machine_id, MachineCommand::Upgrade)?;
    Ok(Ack {})
}

/// Writes only the given settings (never the whole record); a new key reconnects.
pub(super) fn set_settings(conn: &Conn, p: MachinesSetSettingsParams) -> Result<Machine, RpcError> {
    let hub = &conn.hub;
    let machines = hub.repo();
    let machines = machines.machines();
    let id = p.machine_id;
    let before = machines
        .get(id)
        .map_err(store_error)?
        .ok_or_else(|| not_found(Resource::Machine, id))?;
    if let Some(name) = p.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        machines.set_name(id, name).map_err(store_error)?;
    }
    if let Some(on) = p.auto_upgrade {
        machines.set_auto_upgrade(id, on).map_err(store_error)?;
    }
    let mut reconnect = false;
    if let Some(choice) = &p.key {
        if let Some(key_id) = choice.key_id {
            key_for(conn, Some(key_id))?;
        }
        machines.set_key(id, choice.key_id).map_err(store_error)?;
        reconnect = choice.key_id != before.key_id;
    }
    hub.reload_record(id)?;
    if reconnect {
        hub.command(id, MachineCommand::Reconnect)?;
    }
    hub.machine_view(id)
}
