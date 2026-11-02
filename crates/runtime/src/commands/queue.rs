//! Queue commands: send now, edit, remove, reorder, resume.

use controlroom_protocol::daemon::{
    QueueEditParams, QueueRemoveParams, QueueReorderParams, QueueSendNowParams, SendNowResult,
};
use controlroom_protocol::ids::{QueueId, SessionId};
use controlroom_protocol::session::SessionState;
use controlroom_protocol::{Resource, RpcError};

use super::validate_content;
use crate::core::Core;
use crate::errors::{invalid_state, not_found};
use crate::model::{QueueEntry, Via};

impl Core {
    /// `queue.send_now`: the item goes ahead of everything. While the agent is busy it is
    /// steered in when the driver can, otherwise the run is cancelled at a safe point. With
    /// `interrupt` the run is cancelled at once and the item goes next as a prompt (also to
    /// escalate an earlier send-now that is still waiting for its safe point).
    pub(crate) fn send_now(
        &mut self,
        params: &QueueSendNowParams,
    ) -> Result<SendNowResult, RpcError> {
        let id = params.id;
        let session = self.session(id)?;
        if session.data.closed() {
            return Err(invalid_state(SessionState::Closed, "queue.send_now"));
        }
        let entry = session
            .data
            .queue
            .iter()
            .find(|e| e.id == params.queue_id)
            .ok_or_else(|| not_found(Resource::QueueItem, params.queue_id))?;
        if let Some(delivery) = entry.delivery {
            return Ok(result_for(Some(delivery.via()), false));
        }
        let start = (!session.has_process())
            .then(|| super::session::start_mode_of(session))
            .transpose()?;
        let data = &mut self.session_mut(id)?.data;
        let pos = data.queue.iter().position(|e| e.id == params.queue_id);
        let Some(pos) = pos else {
            return Err(not_found(Resource::QueueItem, params.queue_id));
        };
        let mut entry = data.queue.remove(pos);
        entry.send_now = true;
        if params.interrupt {
            entry.interrupt = true;
            entry.no_steer = true;
        }
        let front = data.front();
        data.queue.insert(front, entry);
        if let Some(by) = start {
            self.request_start(id, by)?;
            return Ok(SendNowResult::Queued);
        }
        self.reconcile(id);
        let busy = self.session(id)?.busy();
        Ok(result_for(self.delivery(id, params.queue_id), busy))
    }

    /// `queue.edit`: replace a waiting item's content.
    pub(crate) fn queue_edit(&mut self, params: &QueueEditParams) -> Result<(), RpcError> {
        validate_content(&params.content)?;
        self.waiting_entry(params.id, params.queue_id)?.content = params.content.clone();
        Ok(())
    }

    /// `queue.remove`: drop a waiting item.
    pub(crate) fn queue_remove(&mut self, params: &QueueRemoveParams) -> Result<(), RpcError> {
        self.waiting_entry(params.id, params.queue_id)?;
        let data = &mut self.session_mut(params.id)?.data;
        data.queue.retain(|e| e.id != params.queue_id);
        self.reconcile(params.id);
        Ok(())
    }

    /// `queue.reorder`: move a waiting item; it can't pass items being delivered.
    pub(crate) fn queue_reorder(&mut self, params: &QueueReorderParams) -> Result<(), RpcError> {
        self.waiting_entry(params.id, params.queue_id)?;
        let data = &mut self.session_mut(params.id)?.data;
        let Some(pos) = data.queue.iter().position(|e| e.id == params.queue_id) else {
            return Err(not_found(Resource::QueueItem, params.queue_id));
        };
        let entry = data.queue.remove(pos);
        let to = usize::try_from(params.to_index).unwrap_or(usize::MAX);
        let to = to.clamp(data.front(), data.queue.len());
        data.queue.insert(to, entry);
        self.reconcile(params.id);
        Ok(())
    }

    /// `queue.resume`: restart a paused queue.
    pub(crate) fn queue_resume(&mut self, id: SessionId) -> Result<(), RpcError> {
        self.session_mut(id)?.data.queue_paused = false;
        self.reconcile(id);
        Ok(())
    }

    /// A queue item that hasn't been handed to the agent yet.
    fn waiting_entry(
        &mut self,
        id: SessionId,
        queue_id: QueueId,
    ) -> Result<&mut QueueEntry, RpcError> {
        self.session_mut(id)?
            .data
            .queue
            .iter_mut()
            .find(|e| e.id == queue_id && e.delivery.is_none())
            .ok_or_else(|| not_found(Resource::QueueItem, queue_id))
    }
}

/// What happened to a send-now item: handed over, waiting for the run to stop, or waiting for
/// the agent to be ready.
fn result_for(delivery: Option<Via>, busy: bool) -> SendNowResult {
    match delivery {
        Some(Via::Prompt) => SendNowResult::Sent,
        Some(Via::Steer) => SendNowResult::Steered,
        None if busy => SendNowResult::Interrupting,
        None => SendNowResult::Queued,
    }
}
