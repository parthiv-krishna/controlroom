//! `request.respond`: the first decision wins, and `then` is applied under the same op id.

use controlroom_protocol::daemon::{PromptResult, RequestRespondParams, RequestRespondResult};
use controlroom_protocol::ids::QueueId;
use controlroom_protocol::session::{
    PromptOrigin, Request, RequestAnswer, RequestKind, RequestState, Response,
};
use controlroom_protocol::{ErrorData, Resource, RpcError};

use super::validate_content;
use crate::core::Core;
use crate::errors::{invalid_params, not_found};
use crate::model::{Decision, QueueEntry, Via};

impl Core {
    /// Records the decision and applies `then` as a send-now prompt. An agent's request is
    /// answered by the reconcile loop; a raised one goes back to whoever raised it. The
    /// answer records the deciding device (`by`).
    pub(crate) fn respond(
        &mut self,
        params: &RequestRespondParams,
    ) -> Result<RequestRespondResult, RpcError> {
        if let Some(then) = &params.then {
            validate_content(then)?;
        }
        let id = params.id;
        let request = if self
            .session(id)?
            .data
            .raised
            .contains_key(&params.request_id)
        {
            self.answer_raised(params)?
        } else {
            self.answer_agent(params)?
        };
        let then = match &params.then {
            Some(content) => {
                let entry = QueueEntry {
                    id: QueueId::generate(),
                    content: content.clone(),
                    origin: PromptOrigin::User {
                        device: params.by.clone(),
                    },
                    op_id: params.op_id.clone(),
                    queued_at: self.clock.now(),
                    direct: false,
                    send_now: true,
                    interrupt: false,
                    no_steer: false,
                    delivery: None,
                };
                let queue_id = entry.id;
                self.enqueue(id, entry, true)?;
                self.reconcile(id);
                Some(match self.delivery(id, queue_id) {
                    Some(Via::Prompt) => PromptResult::Sent,
                    Some(Via::Steer) => PromptResult::Steered,
                    None => PromptResult::Queued { queue_id },
                })
            }
            None => {
                self.reconcile(id);
                None
            }
        };
        Ok(RequestRespondResult { request, then })
    }

    /// Answers a request the daemon raised: the first resolution wins.
    fn answer_raised(&mut self, params: &RequestRespondParams) -> Result<Request, RpcError> {
        let session = self.session(params.id)?;
        let Some(raised) = session.data.raised.get(&params.request_id) else {
            return Err(not_found(Resource::Request, &params.request_id));
        };
        let mut request = raised.request.clone();
        if request.state != RequestState::Pending {
            return Err(already_resolved(request));
        }
        check_response(&request, &params.response)?;
        request.state = RequestState::Answered;
        request.answer = Some(self.answer(&request, params));
        self.settle_raised(params.id, request.clone())?;
        Ok(request)
    }

    fn answer(&self, request: &Request, params: &RequestRespondParams) -> RequestAnswer {
        RequestAnswer {
            label: answer_label(request, &params.response),
            response: params.response.clone(),
            by: params.by.clone(),
            at: self.clock.now(),
        }
    }

    /// Records the decision on one of the agent's requests (the reconcile loop writes it).
    fn answer_agent(&mut self, params: &RequestRespondParams) -> Result<Request, RpcError> {
        let id = params.id;
        let session = self.session(id)?;
        if let Some(decision) = session.data.decisions.get(&params.request_id) {
            return Err(already_resolved(decision.request.clone()));
        }
        let mut request = session
            .derived
            .requests
            .get(&params.request_id)
            .cloned()
            .ok_or_else(|| not_found(Resource::Request, &params.request_id))?;
        let open =
            session.derived.process.live && session.view().open_requests.contains(&request.id);
        if request.state != RequestState::Pending || !open {
            if request.state == RequestState::Pending {
                request.state = RequestState::Withdrawn;
            }
            return Err(already_resolved(request));
        }
        check_response(&request, &params.response)?;
        request.state = RequestState::Answered;
        request.answer = Some(self.answer(&request, params));
        let decision = Decision {
            request: request.clone(),
            op_id: Some(params.op_id.clone()),
        };
        self.session_mut(id)?
            .data
            .decisions
            .insert(request.id.clone(), decision);
        Ok(request)
    }
}

fn already_resolved(request: Request) -> RpcError {
    let message = format!("request {} was already resolved", request.id);
    RpcError::new(
        ErrorData::AlreadyResolved {
            request: Box::new(request),
        },
        message,
    )
}

/// The response must fit the request: one of its options, answers to its questions, or a
/// dismissal of a question.
fn check_response(request: &Request, response: &Response) -> Result<(), RpcError> {
    let valid = match response {
        Response::Choice { option_id, .. } => request.options.iter().any(|o| o.id == *option_id),
        Response::Answers { answers } => {
            !request.questions.is_empty()
                && answers
                    .iter()
                    .all(|a| request.questions.iter().any(|q| q.id == a.question_id))
        }
        Response::Dismiss => request.kind == RequestKind::Question,
        Response::Unknown => false,
    };
    if valid {
        Ok(())
    } else {
        Err(invalid_params(format!(
            "the response doesn't fit request {}",
            request.id
        )))
    }
}

/// A short human summary of an answer, e.g. "Allow once".
fn answer_label(request: &Request, response: &Response) -> String {
    match response {
        Response::Choice { option_id, .. } => request
            .options
            .iter()
            .find(|o| o.id == *option_id)
            .map_or_else(|| option_id.clone(), |o| o.label.clone()),
        Response::Answers { answers } => answers
            .iter()
            .flat_map(|a| a.selected.iter().cloned().chain(a.text.clone()))
            .collect::<Vec<_>>()
            .join(", "),
        Response::Dismiss => "Dismissed".into(),
        Response::Unknown => "Answered".into(),
    }
}
