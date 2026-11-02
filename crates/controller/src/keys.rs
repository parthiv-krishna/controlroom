//! SSH keys: the owner's controlroom key (generated on first start, rotatable) and additional
//! keys (generated or imported), sealed under the master key in the store.

use controlroom_client_core::SshPrivateKey;
use controlroom_client_core::keys::install_command;
use controlroom_client_core::seal::{seal_private_key, unseal_private_key};
use controlroom_protocol::ids::KeyId;
use controlroom_protocol::ui::{AdditionalKey, ControlroomKey};
use controlroom_protocol::{ErrorData, RpcError};

use crate::config::Services;
use crate::errors::{internal, store_error};
use crate::store::{KeyKind, KeyRow, NewKey, OwnerId};

/// Seals `key` under a fresh id.
pub(crate) fn seal(
    services: &Services,
    name: &str,
    kind: KeyKind,
    generated: bool,
    key: &SshPrivateKey,
) -> Result<NewKey, RpcError> {
    let id = KeyId::generate();
    let sealed = seal_private_key(&services.master_key, &id.to_string(), key)
        .map_err(|e| internal(format!("sealing the key: {e}")))?;
    Ok(NewKey {
        id,
        name: name.to_owned(),
        kind,
        generated,
        public_key: key.public_key_line(),
        fingerprint: key.fingerprint(),
        sealed_private_key: sealed,
    })
}

/// A fresh ed25519 key with the deployment's comment.
pub(crate) fn generate(services: &Services) -> Result<SshPrivateKey, RpcError> {
    SshPrivateKey::generate_ed25519(&services.key_comment)
        .map_err(|e| internal(format!("generating a key: {e}")))
}

/// The owner's controlroom key, generated on first use.
pub(crate) fn ensure_controlroom_key(
    services: &Services,
    owner: &OwnerId,
) -> Result<KeyRow, RpcError> {
    let keys = services.store.owner(owner);
    let keys = keys.keys();
    if let Some(key) = keys.controlroom().map_err(store_error)? {
        return Ok(key);
    }
    let new = seal(
        services,
        "controlroom",
        KeyKind::Controlroom,
        true,
        &generate(services)?,
    )?;
    let stored = keys.insert(new).map_err(store_error)?;
    tracing::info!(%owner, fingerprint = %stored.fingerprint, "generated the controlroom key");
    Ok(stored)
}

/// Decrypts a stored key in memory.
pub(crate) fn unseal(services: &Services, key: &KeyRow) -> Result<SshPrivateKey, RpcError> {
    unseal_private_key(
        &services.master_key,
        &key.id.to_string(),
        &key.sealed_private_key,
    )
    .map_err(|e| internal(format!("unsealing key {}: {e}", key.fingerprint)))
}

/// The protocol view of the controlroom key.
pub(crate) fn controlroom_view(key: &KeyRow, retiring_machines: u32) -> ControlroomKey {
    ControlroomKey {
        fingerprint: key.fingerprint.clone(),
        public_key: key.public_key.clone(),
        created_at: key.created_at,
        install_command: install_command(&key.public_key),
        retiring_machines,
    }
}

/// The protocol view of an additional key.
pub(crate) fn additional_view(key: &KeyRow) -> AdditionalKey {
    AdditionalKey {
        id: key.id,
        name: key.name.clone(),
        imported: !key.generated,
        fingerprint: key.fingerprint.clone(),
        public_key: key.public_key.clone(),
        created_at: key.created_at,
    }
}

/// An imported key that doesn't parse or decrypt.
pub(crate) fn invalid_key(detail: impl Into<String>) -> RpcError {
    let detail = detail.into();
    RpcError::new(
        ErrorData::InvalidKey {
            detail: detail.clone(),
        },
        detail,
    )
}
