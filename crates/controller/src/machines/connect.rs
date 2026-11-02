//! Connecting to a machine over SSH: which keys to try, and how SSH failures map to
//! connectivity.

use controlroom_client_core::SshPrivateKey;
use controlroom_client_core::ssh::{
    self, ConnectOptions, HostKey, SshConnection, SshError, SshTarget,
};
use controlroom_protocol::RpcError;
use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::{KeyId, MachineId};
use controlroom_protocol::ui::{ActionRequiredReason, Connectivity};

use crate::errors::{internal, not_found, store_error};
use crate::hub::{Hub, host_key_info};
use crate::keys;
use crate::store::KeyKind;

/// The target, pinned host key and key candidates of one attempt.
pub(crate) struct ConnectPlan {
    pub target: SshTarget,
    pub pinned: Option<HostKey>,
    /// `(store key id, key)`, in the order to try.
    pub keys: Vec<(KeyId, SshPrivateKey)>,
}

/// The machine's additional key, or the controlroom key installed on it (a retiring one after
/// a rotation) followed by the current one.
pub(crate) fn plan(hub: &Hub, id: MachineId) -> Result<ConnectPlan, RpcError> {
    let repo = hub.repo();
    let record = repo
        .machines()
        .get(id)
        .map_err(store_error)?
        .ok_or_else(|| not_found(Resource::Machine, id))?;
    let pinned = record
        .host_key
        .as_deref()
        .map(HostKey::parse)
        .transpose()
        .map_err(|e| internal(e.to_string()))?;
    let mut candidates = Vec::new();
    match record.key_id {
        Some(key_id) => candidates.push(key_id),
        None => {
            candidates.extend(record.auth_key_id);
            let current = keys::ensure_controlroom_key(&hub.services, &hub.owner)?;
            if !candidates.contains(&current.id) {
                candidates.push(current.id);
            }
        }
    }
    let mut out = Vec::new();
    for key_id in candidates {
        if let Some(key) = repo.keys().get(key_id).map_err(store_error)? {
            out.push((key.id, keys::unseal(&hub.services, &key)?));
        }
    }
    Ok(ConnectPlan {
        target: SshTarget::new(record.host, record.port, record.username),
        pinned,
        keys: out,
    })
}

/// Tries each key in turn; only authentication failures move on to the next.
pub(crate) async fn connect_any(
    plan: &ConnectPlan,
    options: &ConnectOptions,
) -> Result<(SshConnection, KeyId), SshError> {
    let mut last = None;
    for (key_id, key) in &plan.keys {
        match ssh::connect(&plan.target, key, plan.pinned.as_ref(), options).await {
            Ok(conn) => return Ok((conn, *key_id)),
            Err(e @ SshError::AuthFailed { .. }) => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| SshError::Other("no SSH key is available for this machine".into())))
}

/// The state an SSH failure leaves a machine in: `action_required` for host-key and
/// authentication problems (no retry until the user acts), else `unreachable`.
pub(crate) fn failure_connectivity(e: &SshError) -> Connectivity {
    let action = |reason, presented: Option<&HostKey>| Connectivity::ActionRequired {
        reason,
        presented_host_key: presented.map(host_key_info),
    };
    match e {
        SshError::UnknownHostKey { host_key } => {
            action(ActionRequiredReason::HostKeyUnconfirmed, Some(host_key))
        }
        SshError::HostKeyMismatch { actual, .. } => {
            action(ActionRequiredReason::HostKeyChanged, Some(actual))
        }
        SshError::AuthFailed { .. } => action(ActionRequiredReason::AuthFailed, None),
        _ => Connectivity::Unreachable,
    }
}

/// Remembers which controlroom key a machine on the controlroom key authenticated with, and
/// deletes retired keys nothing uses anymore.
pub(crate) fn record_auth_key(hub: &Hub, id: MachineId, key_id: KeyId) {
    let repo = hub.repo();
    let Ok(Some(record)) = repo.machines().get(id) else {
        return;
    };
    if record.key_id.is_some() || record.auth_key_id == Some(key_id) {
        return;
    }
    let controlroom = repo
        .keys()
        .get(key_id)
        .ok()
        .flatten()
        .is_some_and(|k| k.kind == KeyKind::Controlroom);
    if controlroom {
        if let Err(e) = repo.machines().set_auth_key(id, Some(key_id)) {
            tracing::warn!(error = %e, "recording the machine's key");
        }
        if let Err(e) = repo.keys().prune_retired() {
            tracing::warn!(error = %e, "pruning retired keys");
        }
    }
}
