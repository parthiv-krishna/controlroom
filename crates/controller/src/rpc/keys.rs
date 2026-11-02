//! SSH keys (`keys.*`; rotation is in [`crate::rotation`]).

use controlroom_client_core::SshPrivateKey;
use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::MachineId;
use controlroom_protocol::ui::{
    AdditionalKey, KeysDeleteParams, KeysGenerateParams, KeysImportParams, KeysListResult,
};
use controlroom_protocol::{Ack, ErrorData, RpcError};

use super::Conn;
use crate::errors::{not_found, store_error};
use crate::keys::{self, additional_view, controlroom_view, invalid_key};
use crate::store::KeyKind;

pub(super) fn list(conn: &Conn) -> Result<KeysListResult, RpcError> {
    let current = keys::ensure_controlroom_key(&conn.hub.services, &conn.hub.owner)?;
    let repo = conn.hub.repo();
    let retiring = repo
        .machines()
        .list()
        .map_err(store_error)?
        .iter()
        .filter(|m| m.key_id.is_none() && m.auth_key_id.is_some_and(|a| a != current.id))
        .count() as u32;
    let additional = repo
        .keys()
        .list()
        .map_err(store_error)?
        .iter()
        .filter(|k| k.kind == KeyKind::Additional)
        .map(additional_view)
        .collect();
    Ok(KeysListResult {
        controlroom: controlroom_view(&current, retiring),
        additional,
    })
}

fn store(
    conn: &Conn,
    name: &str,
    generated: bool,
    key: &SshPrivateKey,
) -> Result<AdditionalKey, RpcError> {
    let new = keys::seal(
        &conn.hub.services,
        name,
        KeyKind::Additional,
        generated,
        key,
    )?;
    let stored = conn.hub.repo().keys().insert(new).map_err(store_error)?;
    Ok(additional_view(&stored))
}

pub(super) fn generate(conn: &Conn, p: KeysGenerateParams) -> Result<AdditionalKey, RpcError> {
    store(conn, &p.name, true, &keys::generate(&conn.hub.services)?)
}

/// The passphrase decrypts the key once and is discarded.
pub(super) fn import(conn: &Conn, p: KeysImportParams) -> Result<AdditionalKey, RpcError> {
    let key = SshPrivateKey::import(&p.private_key, p.passphrase.as_deref())
        .map_err(|e| invalid_key(e.to_string()))?;
    store(conn, &p.name, false, &key)
}

pub(super) fn delete(conn: &Conn, p: KeysDeleteParams) -> Result<Ack, RpcError> {
    let repo = conn.hub.repo();
    let key = repo
        .keys()
        .get(p.key_id)
        .map_err(store_error)?
        .filter(|k| k.kind == KeyKind::Additional)
        .ok_or_else(|| not_found(Resource::Key, p.key_id))?;
    let users: Vec<MachineId> = repo
        .machines()
        .list()
        .map_err(store_error)?
        .iter()
        .filter(|m| m.key_id == Some(key.id))
        .map(|m| m.id)
        .collect();
    if !users.is_empty() {
        let message = format!("{} machine(s) use this key", users.len());
        return Err(RpcError::new(
            ErrorData::KeyInUse { machine_ids: users },
            message,
        ));
    }
    repo.keys().delete(key.id).map_err(store_error)?;
    Ok(Ack {})
}
