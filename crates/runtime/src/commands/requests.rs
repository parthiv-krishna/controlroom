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
    /// Records the decision (the reconcile loop writes the answer) and applies `then` as a
    /// send-now prompt.
    pub(crate) fn respond(
        &mut self,
        params: &RequestRespondParams,
    ) -> Result<RequestRespondResult, RpcError> {
        if let Some(then) = &params.then {
            validate_content(then)?;
        }
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
        request.answer = Some(RequestAnswer {
            label: answer_label(&request, &params.response),
            response: params.response.clone(),
            by: None,
            at: self.clock.now(),
        });
        let decision = Decision {
            request: request.clone(),
            op_id: Some(params.op_id.clone()),
        };
        self.session_mut(id)?
            .data
            .decisions
            .insert(request.id.clone(), decision);
        let then = match &params.then {
            Some(content) => {
                let entry = QueueEntry {
                    id: QueueId::generate(),
                    content: content.clone(),
                    origin: PromptOrigin::default(),
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
