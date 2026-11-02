//! Rotating the controlroom key (`keys.rotate`).
//!
//! The store change is one transaction: the new key is stored, the old one retired, and
//! machines that never recorded their key are pinned to the old one. Then every connected
//! machine on the controlroom key is migrated over its existing connection: append the new
//! public key to `authorized_keys`, prove it authenticates, remove the old line, and only then
//! record the new key. A machine whose migration fails (unreachable, or the old line couldn't
//! be removed) keeps its old key recorded and is migrated again when it next connects; a
//! retired key is deleted once no machine records it.
//!
//! Migrations after a reconnect run in their own task, so a long rotation never holds up a
//! machine's connection loop.

use std::sync::Arc;

use controlroom_client_core::bootstrap::rotate_key;
use controlroom_client_core::ssh::SshTarget;
use controlroom_protocol::RpcError;
use controlroom_protocol::ids::MachineId;
use controlroom_protocol::ui::KeysRotateResult;

use crate::errors::{internal, store_error};
use crate::hub::Hub;
use crate::keys::{self, controlroom_view};
use crate::store::KeyKind;

/// `keys.rotate`.
pub(crate) async fn rotate(hub: &Arc<Hub>) -> Result<KeysRotateResult, RpcError> {
    let _guard = hub.rotation.lock().await;
    let services = &hub.services;
    let old = keys::ensure_controlroom_key(services, &hub.owner)?;
    let new = keys::seal(
        services,
        "controlroom",
        KeyKind::Controlroom,
        true,
        &keys::generate(services)?,
    )?;
    let repo = hub.repo();
    repo.keys()
        .rotate_controlroom(old.id, new)
        .map_err(store_error)?;
    let current = repo
        .keys()
        .controlroom()
        .map_err(store_error)?
        .ok_or_else(|| internal("the new controlroom key vanished"))?;
    tracing::info!(old = %old.fingerprint, new = %current.fingerprint, "rotating the controlroom key");

    let machines: Vec<_> = repo
        .machines()
        .list()
        .map_err(store_error)?
        .into_iter()
        .filter(|m| m.key_id.is_none())
        .map(|m| m.id)
        .collect();
    let (mut migrated, mut pending) = (0, 0);
    for id in machines {
        match migrate(hub, id).await {
            Ok(true) => migrated += 1,
            Ok(false) => pending += 1,
            Err(e) => {
                tracing::warn!(machine = %id, error = %e, "migrating to the new key");
                pending += 1;
            }
        }
    }
    repo.keys().prune_retired().map_err(store_error)?;
    Ok(KeysRotateResult {
        key: controlroom_view(&current, pending),
        migrated,
        pending,
    })
}

/// Finishes a pending migration once the machine is connected (spawned by its task).
pub(crate) async fn migrate_pending(hub: Arc<Hub>, id: MachineId) {
    let _guard = hub.rotation.lock().await;
    match migrate(&hub, id).await {
        Ok(true) => {
            if let Err(e) = hub.repo().keys().prune_retired() {
                tracing::warn!(error = %e, "pruning retired keys");
            }
        }
        Ok(false) => {}
        Err(e) => tracing::warn!(machine = %id, error = %e, "migrating to the new controlroom key"),
    }
}

/// Moves one connected machine to the current controlroom key. `Ok(false)` if it isn't
/// connected; `Ok(true)` if it is (now) on the current key or doesn't use the controlroom key.
async fn migrate(hub: &Hub, id: MachineId) -> Result<bool, String> {
    let repo = hub.repo();
    let record = repo
        .machines()
        .get(id)
        .map_err(|e| e.to_string())?
        .ok_or("the machine was removed")?;
    if record.key_id.is_some() {
        return Ok(true);
    }
    let current = repo
        .keys()
        .controlroom()
        .map_err(|e| e.to_string())?
        .ok_or("no controlroom key")?;
    let Some((ssh, used)) = hub.link_ssh(id) else {
        return Ok(false);
    };
    let old_id = record.auth_key_id.unwrap_or(used);
    if old_id == current.id {
        return Ok(true);
    }
    let old = repo
        .keys()
        .get(old_id)
        .map_err(|e| e.to_string())?
        .ok_or("the machine's key is gone")?;
    let new_key = keys::unseal(&hub.services, &current).map_err(|e| e.message)?;
    let target = SshTarget::new(record.host, record.port, record.username);
    let proof = rotate_key(
        &ssh,
        &target,
        &new_key,
        &old.public_key,
        &hub.services.tuning.ssh,
    )
    .await
    .map_err(|e| e.to_string())?;
    proof.disconnect().await;
    repo.machines()
        .set_auth_key(id, Some(current.id))
        .map_err(|e| e.to_string())?;
    tracing::info!(machine = %id, "migrated to the new controlroom key");
    Ok(true)
}
