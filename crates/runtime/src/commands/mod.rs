//! The daemon API's mutating commands, run inside a transaction under the runtime's lock.
//!
//! Every command goes through [`Core::with_op`]: the op id is recorded in the same transaction
//! as the command's effect, a repeat returns the stored result, and an op id reused for a
//! different command is `op_id_conflict`. A failed command records nothing.

mod queue;
mod requests;
mod sandbox;
mod session;

pub(crate) use session::start_mode_of;

use controlroom_protocol::ids::{OpId, SessionId};
use controlroom_protocol::session::{ContentPart, StartedBy};
use controlroom_protocol::{ErrorData, RpcError};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::core::{Core, Task};
use crate::errors::{internal, invalid_params};
use crate::store::{StoredOp, json};

impl Core {
    /// Runs `f` once per op id (see the module docs).
    pub(crate) fn with_op<P: Serialize, R: Serialize + DeserializeOwned>(
        &mut self,
        method: &str,
        op_id: &OpId,
        params: &P,
        f: impl FnOnce(&mut Core) -> Result<R, RpcError>,
    ) -> Result<R, RpcError> {
        let hash = params_hash(params);
        if let Some(result) = self.stored_op(method, op_id, &hash)? {
            return Ok(result);
        }
        let result = f(self)?;
        let now = self.clock.now();
        self.store
            .put_op(op_id, method, &hash, &json(&result), now)
            .map_err(internal)?;
        Ok(result)
    }

    /// The stored result of this exact command, if it already ran.
    pub(crate) fn stored_op<R: DeserializeOwned>(
        &self,
        method: &str,
        op_id: &OpId,
        hash: &str,
    ) -> Result<Option<R>, RpcError> {
        match self.store.op(op_id, method, hash).map_err(internal)? {
            StoredOp::New => Ok(None),
            StoredOp::Done(result) => serde_json::from_str(&result).map(Some).map_err(internal),
            StoredOp::Conflict => Err(RpcError::new(
                ErrorData::OpIdConflict,
                format!("op_id {op_id} was already used for a different command"),
            )),
        }
    }

    /// Starts the agent with `pending_start`, unless a start is already running. While the
    /// previous supervisor is still attached (it holds the transcript until it has drained its
    /// readers), the start waits for it to go.
    pub(crate) fn request_start(&mut self, id: SessionId, by: StartedBy) -> Result<(), RpcError> {
        let session = self.session_mut(id)?;
        session.data.pending_start = Some(by);
        session.live.launch_error = None;
        if session.live.launching {
            return Ok(());
        }
        if session.live.attachment.is_some() {
            session.live.relaunch = true;
            return Ok(());
        }
        session.live.launching = true;
        self.task(Task::Launch(id));
        Ok(())
    }
}

/// A stable hash of a command's params (FNV-1a over their JSON), so op dedup can tell a repeat
/// from a reuse without storing prompt contents.
pub(crate) fn params_hash<P: Serialize>(params: &P) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in json(params).bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Prompt content must have at least one part.
pub(crate) fn validate_content(content: &[ContentPart]) -> Result<(), RpcError> {
    if content.is_empty() {
        return Err(invalid_params("content is empty"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_hash_is_stable() {
        assert_eq!(params_hash(&"x"), params_hash(&"x"));
        assert_ne!(params_hash(&"x"), params_hash(&"y"));
        // FNV-1a of the empty JSON object; fixed forever, because hashes are persisted.
        let empty = serde_json::json!({});
        assert_eq!(params_hash(&empty), "08f44b07b5901a25");
    }
}
