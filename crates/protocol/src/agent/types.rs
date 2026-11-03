//! Params, results and notifications of the agent API.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{RequestId, SessionId, TodoId};
use crate::session::{AutoContinueInfo, Request, RequestAnswer, RequestState, Todo, TodoStatus};
use crate::timeline::NoticeLevel;
use crate::version::{Capability, ProtocolVersion};

/// Params of the agent `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentHelloParams {
    /// The protocol version the extension speaks.
    pub protocol_version: ProtocolVersion,
    /// The session's token (`CONTROLROOM_AGENT_TOKEN`).
    pub token: String,
    /// A short label for logs, e.g. `"@controlroom/pi 0.0.5"`.
    pub client_name: String,
}

/// Result of the agent `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentHelloResult {
    /// The `controlroomd` software version.
    pub daemon_version: String,
    /// The agent protocol version the daemon speaks (same major as the extension's).
    pub protocol_version: ProtocolVersion,
    /// The session this socket belongs to.
    pub session_id: SessionId,
    /// Optional features; the extension ignores unknown ones.
    pub capabilities: Vec<Capability>,
}

/// One choice the agent suggests for a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentQuestionOption {
    /// The label, also the answer value.
    pub label: String,
    /// Optional explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One question of `question.ask`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentQuestion {
    /// The question (Markdown).
    pub text: String,
    /// Suggested choices; empty for a free-text question.
    #[serde(default)]
    pub options: Vec<AgentQuestionOption>,
    /// Several choices may be selected. Default `false`.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub multi: bool,
    /// A free-text answer is allowed. Default `true`.
    #[serde(default = "yes")]
    #[ts(as = "Option<bool>", optional)]
    pub free_text: bool,
}

fn yes() -> bool {
    true
}

/// Params of `question.ask`: questions the user answers asynchronously. Never blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QuestionAskParams {
    /// Idempotency key: the pi tool call id.
    pub call_id: String,
    /// A one-line summary; default: the first question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// One or more questions, answered together.
    pub questions: Vec<AgentQuestion>,
}

/// Result of `question.ask`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QuestionAskResult {
    /// The question's id, for `question.withdraw`; the answer refers to it.
    pub question_id: RequestId,
    /// The reminder for the model (spec §8): keep working; the answer will wake you.
    pub reminder: String,
}

/// Params of `question.withdraw`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QuestionWithdrawParams {
    /// Idempotency key: the pi tool call id.
    pub call_id: String,
    /// The question.
    pub question_id: RequestId,
}

/// Result of `question.withdraw`: the question's final state. Withdrawing a question the user
/// already answered changes nothing (the answer is, or will be, delivered).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QuestionWithdrawResult {
    /// `withdrawn`, or how it was resolved before.
    pub state: RequestState,
    /// The user's answer, if it was answered first.
    pub answer: Option<RequestAnswer>,
}

/// The todo list after a `todo.*` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentTodos {
    /// Every entry, in display order.
    pub todos: Vec<Todo>,
}

/// Params of the agent's `todo.add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentTodoAddParams {
    /// Idempotency key: the pi tool call id.
    pub call_id: String,
    /// One new `pending` entry per text, in order.
    pub texts: Vec<String>,
}

/// Params of the agent's `todo.update`. Absent fields are left unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentTodoUpdateParams {
    /// Idempotency key: the pi tool call id.
    pub call_id: String,
    /// The entry.
    pub todo_id: TodoId,
    /// New text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// New status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TodoStatus>,
    /// Why it is blocked (or skipped); `""` clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Params of `notify`: a notice for the user in the session's timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NotifyParams {
    /// Idempotency key: the pi tool call id.
    pub call_id: String,
    /// The text (Markdown).
    pub text: String,
    /// Severity; default `info`.
    #[serde(default = "info")]
    #[ts(as = "Option<NoticeLevel>", optional)]
    pub level: NoticeLevel,
}

fn info() -> NoticeLevel {
    NoticeLevel::Info
}

/// The `state` notification: what the session's controlroom side looks like now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentState {
    /// The todo list, in display order.
    pub todos: Vec<Todo>,
    /// The agent's questions that are still open.
    pub questions: Vec<Request>,
    /// Auto-continue.
    pub auto_continue: AutoContinueInfo,
}
