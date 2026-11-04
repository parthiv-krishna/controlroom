//! Requests the daemon raises for a session itself, outside the agent's transcript: the agent's
//! own questions (`ask_user`, see `questions`) and its workspaces' host requests (sandbox
//! approvals and commands waiting for terminal input, see `hosts::approvals`).
//!
//! A raised request is persisted with the session, shown like any other request (summary
//! `requests[]`, attention, state `awaiting_permission`, a timeline `request` item) and
//! answered with `request.respond`. The first resolution wins: an answer, a withdrawal (the run
//! was cancelled, the session closed, the command moved on) or an expiry (the host gave up).
//! A host approval's resolution goes back to its host; a question's answer is injected into
//! the agent's next turn.
//!
//! Timeline placement: see `items`.

use controlroom_protocol::RpcError;
use controlroom_protocol::ids::{ItemId, SessionId};
use controlroom_protocol::session::{Request, RequestState, SessionState};
use controlroom_protocol::timeline::ItemBody;

use crate::core::Core;
use crate::errors::invalid_state;
use crate::model::{HostRequestRef, Raised};

impl Core {
    /// Raises a request for one of the session's hosts (a no-op if it is already raised).
    pub(crate) fn raise_host(
        &mut self,
        id: SessionId,
        request: Request,
        host: HostRequestRef,
    ) -> Result<(), RpcError> {
        let seq = self.head(id);
        let session = self.session_mut(id)?;
        if session.data.closed() {
            return Err(invalid_state(SessionState::Closed, "raising a request"));
        }
        if session.data.raised.contains_key(&request.id) {
            return Ok(());
        }
        let raised = Raised {
            request,
            seq,
            host: Some(host),
        };
        session
            .data
            .raised
            .insert(raised.request.id.clone(), raised.clone());
        self.publish_raised(id, &raised)
    }

    /// Stores a raised request's new state and updates its timeline item; a host approval's
    /// resolution goes back to its host. `request` must be resolved (no longer pending).
    pub(crate) fn settle_raised(
        &mut self,
        id: SessionId,
        request: Request,
    ) -> Result<(), RpcError> {
        let session = self.session_mut(id)?;
        let Some(raised) = session.data.raised.get_mut(&request.id) else {
            return Ok(());
        };
        raised.request = request;
        let raised = raised.clone();
        self.publish_raised(id, &raised)?;
        self.host_request_settled(id, &raised);
        Ok(())
    }

    /// Ends the session's pending host requests (and with `questions`, its open questions too)
    /// with `state`: `withdrawn` when a cancel or close made them moot.
    pub(crate) fn end_pending_raised(
        &mut self,
        id: SessionId,
        state: RequestState,
        questions: bool,
    ) -> Result<(), RpcError> {
        let pending: Vec<Request> = self
            .session(id)?
            .data
            .raised
            .values()
            .filter(|r| r.request.state == RequestState::Pending)
            .filter(|r| questions || !r.is_question())
            .map(|r| r.request.clone())
            .collect();
        for mut request in pending {
            request.state = state;
            self.settle_raised(id, request)?;
        }
        Ok(())
    }

    /// Writes the raised request's timeline item and sends it to the session's subscribers.
    pub(crate) fn publish_raised(
        &mut self,
        id: SessionId,
        raised: &Raised,
    ) -> Result<(), RpcError> {
        self.put_item_at(
            id,
            ItemId::new(raised.request.id.as_str()),
            ItemBody::Request {
                request: raised.request.clone(),
            },
            raised.seq,
            raised.request.created_at,
        )
    }
}
