//! The agent's questions (`ask_user`, spec §8): always non-blocking.
//!
//! A question is a raised request of kind `question` (`blocking: false`): it shows in the
//! summary's `requests[]` with the lighter `question` attention, as a timeline `request` item,
//! and is answered with `request.respond` (answers, or Dismiss, which counts as "declined").
//! Nobody waits for it: the answer is injected into the agent's next turn (a steer while it
//! works, a new run while it is idle), and the agent can withdraw it. Questions stay open
//! across daemon restarts; closing the session withdraws them.

use controlroom_protocol::RpcError;
use controlroom_protocol::agent::{
    AgentQuestion, QuestionAskParams, QuestionAskResult, QuestionWithdrawParams,
    QuestionWithdrawResult,
};
use controlroom_protocol::ids::{RequestId, SessionId};
use controlroom_protocol::session::{
    Question, QuestionOption, Request, RequestKind, RequestState, Response, SessionState,
};
use controlroom_protocol::timeline::InjectOrigin;
use controlroom_protocol::{Resource, UnixMillis};

use crate::core::Core;
use crate::errors::{invalid_params, invalid_state, not_found};
use crate::model::{Injection, Raised};
use crate::preview::one_line;

/// What `ask_user` tells the model (spec §8).
pub(crate) const REMINDER: &str = "Question sent; the user will answer asynchronously. Keep \
     working on other items. End your turn only if you truly can't make progress; the answer \
     will wake you.";

/// Questions per ask, and choices per question.
const MAX_QUESTIONS: usize = 8;
const MAX_OPTIONS: usize = 12;

impl Core {
    /// `question.ask`: raises a question on the session.
    pub(crate) fn ask(
        &mut self,
        id: SessionId,
        params: &QuestionAskParams,
    ) -> Result<QuestionAskResult, RpcError> {
        let questions = questions(&params.questions)?;
        let now = self.clock.now();
        let seq = self.head(id);
        let session = self.session_mut(id)?;
        if session.data.closed() {
            return Err(invalid_state(SessionState::Closed, "question.ask"));
        }
        let title = params
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map_or_else(|| one_line(&questions[0].text), one_line);
        let request = Request {
            id: RequestId::new(format!("question-{}", ulid::Ulid::generate())),
            kind: RequestKind::Question,
            blocking: false,
            state: RequestState::Pending,
            title,
            detail: None,
            body: None,
            options: Vec::new(),
            questions,
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
        Ok(QuestionAskResult {
            question_id: request.id,
            reminder: REMINDER.into(),
        })
    }

    /// `question.withdraw`: withdraws an open question; an answered one stays answered.
    pub(crate) fn withdraw(
        &mut self,
        id: SessionId,
        params: &QuestionWithdrawParams,
    ) -> Result<QuestionWithdrawResult, RpcError> {
        let raised = self
            .session(id)?
            .data
            .raised
            .get(&params.question_id)
            .filter(|r| r.is_question())
            .ok_or_else(|| not_found(Resource::Request, &params.question_id))?;
        let mut request = raised.request.clone();
        if request.state == RequestState::Pending {
            request.state = RequestState::Withdrawn;
            self.settle_raised(id, request.clone())?;
        }
        Ok(QuestionWithdrawResult {
            state: request.state,
            answer: request.answer,
        })
    }

    /// The user answered (or dismissed) a question: the answer goes to the agent.
    pub(crate) fn question_answered(
        &mut self,
        id: SessionId,
        request: &Request,
        now: UnixMillis,
    ) -> Result<(), RpcError> {
        let Some(answer) = &request.answer else {
            return Ok(());
        };
        let text = answer_text(request, &answer.response);
        let label = format!("answer: {}", request.title);
        let injection = Injection::new(InjectOrigin::Answer, label, text, true, now);
        self.inject(id, injection)
    }
}

/// Validates and converts the agent's questions; ids are `q1`, `q2`, ...
fn questions(asked: &[AgentQuestion]) -> Result<Vec<Question>, RpcError> {
    if asked.is_empty() || asked.len() > MAX_QUESTIONS {
        return Err(invalid_params(format!(
            "ask between 1 and {MAX_QUESTIONS} questions"
        )));
    }
    asked
        .iter()
        .enumerate()
        .map(|(n, q)| {
            let text = q.text.trim();
            if text.is_empty() {
                return Err(invalid_params("a question's text is empty"));
            }
            if q.options.len() > MAX_OPTIONS {
                return Err(invalid_params(format!(
                    "at most {MAX_OPTIONS} options per question"
                )));
            }
            let options: Vec<QuestionOption> = q
                .options
                .iter()
                .filter(|o| !o.label.trim().is_empty())
                .map(|o| QuestionOption {
                    label: o.label.trim().to_owned(),
                    description: o.description.clone().filter(|d| !d.trim().is_empty()),
                })
                .collect();
            Ok(Question {
                id: format!("q{}", n + 1),
                text: text.to_owned(),
                free_text: q.free_text || options.is_empty(),
                multi: q.multi && !options.is_empty(),
                options,
            })
        })
        .collect()
}

/// The injected answer: every question with its answer, or "declined".
fn answer_text(request: &Request, response: &Response) -> String {
    let head = format!(
        "[controlroom] The user answered your question {} (\"{}\"):",
        request.id, request.title
    );
    let Response::Answers { answers } = response else {
        return format!(
            "[controlroom] The user dismissed your question {} (\"{}\"): declined. Decide on \
             your own and carry on.",
            request.id, request.title
        );
    };
    let mut out = head;
    for question in &request.questions {
        let answer = answers.iter().find(|a| a.question_id == question.id);
        let parts: Vec<String> = answer
            .map(|a| a.selected.iter().cloned().chain(a.text.clone()).collect())
            .unwrap_or_default();
        let parts: Vec<String> = parts.into_iter().filter(|p| !p.trim().is_empty()).collect();
        let answered = if parts.is_empty() {
            "(no answer)".to_owned()
        } else {
            parts.join("; ")
        };
        out.push_str(&format!("\n- {}\n  → {answered}", question.text));
    }
    out
}
