//! Requests the daemon raises for a session itself, outside the agent's transcript: sandbox
//! approvals from nono's webhook (spec §11.4), and the agent's own questions (`ask_user`, see
//! `questions`).
//!
//! A raised request is persisted with the session, shown like any other request (summary
//! `requests[]`, attention, state `awaiting_permission`, a timeline `request` item) and
//! answered with `request.respond`. Its resolution goes back to whoever raised it through a
//! [`RaisedRequest`] handle, never to the agent. The first resolution wins: an answer, a
//! withdrawal (the run was cancelled or the session closed) or an expiry (the handle gave up or
//! was dropped, e.g. because the HTTP request behind it died).
//!
//! Raised approvals don't survive the daemon: whoever waited for them is gone, so recovery
//! expires every approval still pending. Questions have no waiter (their answer is injected)
//! and stay open across restarts.
//!
//! Timeline placement: see `items`.

use std::sync::Arc;

use controlroom_protocol::RpcError;
use controlroom_protocol::ids::{ItemId, RequestId, SessionId};
use controlroom_protocol::session::{
    Request, RequestBody, RequestKind, RequestOption, RequestState, SessionState,
};
use controlroom_protocol::timeline::ItemBody;
use tokio::sync::oneshot;

use crate::core::Core;
use crate::errors::invalid_state;
use crate::model::Raised;
use crate::runtime::Inner;

/// What the daemon asks the user. Raised requests are always blocking approvals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaiseRequest {
    /// What it asks for (e.g. `sandbox`).
    pub kind: RequestKind,
    /// One-line summary, e.g. "Run `git push origin main`?".
    pub title: String,
    /// Longer explanation.
    pub detail: Option<String>,
    /// Structured subject.
    pub body: Option<RequestBody>,
    /// The answer options.
    pub options: Vec<RequestOption>,
}

/// A raised request, as seen by whoever raised it. Dropping it before the request is resolved
/// expires the request.
pub struct RaisedRequest {
    inner: Arc<Inner>,
    session: SessionId,
    request: Request,
    resolution: oneshot::Receiver<Request>,
    settled: bool,
}

impl RaisedRequest {
    /// The request as raised.
    pub fn request(&self) -> &Request {
        &self.request
    }

    /// Waits for the first resolution: answered, withdrawn or expired. Cancel-safe, so it can
    /// be raced against a deadline and followed by [`RaisedRequest::expire`].
    pub async fn resolution(&mut self) -> Request {
        let request = match (&mut self.resolution).await {
            Ok(request) => request,
            // The session was deleted.
            Err(_) => Request {
                state: RequestState::Withdrawn,
                ..self.request.clone()
            },
        };
        self.settled = true;
        request
    }

    /// Gives up: expires the request unless it was resolved first, and returns how it ended.
    pub fn expire(mut self) -> Request {
        self.settled = true;
        if let Ok(request) = self.resolution.try_recv() {
            return request;
        }
        let (session, id) = (self.session, self.request.id.clone());
        match self.inner.transact(|core| core.expire_raised(session, &id)) {
            Ok(Some(request)) => request,
            _ => Request {
                state: RequestState::Expired,
                ..self.request.clone()
            },
        }
    }
}

impl Drop for RaisedRequest {
    fn drop(&mut self) {
        if !self.settled {
            let (session, id) = (self.session, self.request.id.clone());
            if let Err(e) = self.inner.transact(|core| core.expire_raised(session, &id)) {
                tracing::warn!("session {session}: expiring request {id}: {e}");
            }
        }
    }
}

/// Raises `spec` on the session and returns its handle (inside the caller's runtime).
pub(crate) fn raise(
    inner: &Arc<Inner>,
    session: SessionId,
    spec: RaiseRequest,
) -> Result<RaisedRequest, RpcError> {
    let (tx, rx) = oneshot::channel();
    let request = inner.transact(|core| core.raise(session, spec, tx))?;
    Ok(RaisedRequest {
        inner: inner.clone(),
        session,
        request,
        resolution: rx,
        settled: false,
    })
}

impl Core {
    /// Records a new raised request and registers its waiter.
    fn raise(
        &mut self,
        id: SessionId,
        spec: RaiseRequest,
        waiter: oneshot::Sender<Request>,
    ) -> Result<Request, RpcError> {
        let now = self.clock.now();
        let seq = self.head(id);
        let session = self.session_mut(id)?;
        if session.data.closed() {
            return Err(invalid_state(SessionState::Closed, "raising a request"));
        }
        let request = Request {
            id: RequestId::new(format!("daemon-{}", ulid::Ulid::generate())),
            kind: spec.kind,
            blocking: true,
            state: RequestState::Pending,
            title: spec.title,
            detail: spec.detail,
            body: spec.body,
            options: spec.options,
            questions: Vec::new(),
            answer: None,
            origin_child: None,
            created_at: now,
        };
        let raised = Raised {
            request: request.clone(),
            seq,
        };
        session
            .data
            .raised
            .insert(request.id.clone(), raised.clone());
        self.publish_raised(id, &raised)?;
        self.add_waiter((id, request.id.clone()), waiter);
        Ok(request)
    }

    /// Stores a raised request's new state, updates its timeline item and tells its waiter.
    /// `request` must be resolved (no longer pending).
    pub(crate) fn settle_raised(
        &mut self,
        id: SessionId,
        request: Request,
    ) -> Result<(), RpcError> {
        let session = self.session_mut(id)?;
        let Some(raised) = session.data.raised.get_mut(&request.id) else {
            return Ok(());
        };
        raised.request = request.clone();
        let raised = raised.clone();
        self.publish_raised(id, &raised)?;
        self.resolved(id, request);
        Ok(())
    }

    /// Expires a raised request unless it is already resolved; returns its final state
    /// (`None` when the session or request is gone).
    pub(crate) fn expire_raised(
        &mut self,
        id: SessionId,
        request_id: &RequestId,
    ) -> Result<Option<Request>, RpcError> {
        let Some(raised) = self
            .sessions
            .get(&id)
            .and_then(|s| s.data.raised.get(request_id))
        else {
            return Ok(None);
        };
        let mut request = raised.request.clone();
        if request.state == RequestState::Pending {
            request.state = RequestState::Expired;
            self.settle_raised(id, request.clone())?;
        }
        Ok(Some(request))
    }

    /// Ends the session's pending approvals (and with `questions`, its open questions too)
    /// with `state`: `withdrawn` when a cancel or close made them moot, `expired` after a
    /// restart (nobody waits for them).
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
