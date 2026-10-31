//! Runtime commands: answering requests, editing todos, managing jobs and revoking sandbox
//! grants.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::daemon::PromptResult;
use crate::ids::{GrantId, JobId, OpId, RequestId, SessionId, TodoId};
use crate::session::{ContentPart, Job, Request, Response, Todo, TodoStatus};

/// Params of `request.respond`: answer a permission, question, dialog, sandbox, contact or
/// spawn request. The first answer wins; later ones get `already_resolved` with the winner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RequestRespondParams {
    /// Idempotency key; `then` is applied atomically under it.
    pub op_id: OpId,
    /// The session the request belongs to (for a rolled-up request, the child session).
    pub id: SessionId,
    /// The request.
    pub request_id: RequestId,
    /// The answer.
    pub response: Response,
    /// A prompt to send right after, ahead of the queue ("deny and steer").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<Vec<ContentPart>>,
}

/// Result of `request.respond`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RequestRespondResult {
    /// The request with its recorded answer.
    pub request: Request,
    /// What happened to `then`, if given.
    pub then: Option<PromptResult>,
}

/// Params of `todo.add`: add entries (e.g. seeded from a task checklist) atomically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TodoAddParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// One new `pending` entry per text, in order.
    pub texts: Vec<String>,
    /// Insert position; default: the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
}

/// Result of `todo.add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TodoAddResult {
    /// The new entries.
    pub todos: Vec<Todo>,
}

/// Params of `todo.update`. Absent fields are left unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TodoUpdateParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The entry.
    pub todo_id: TodoId,
    /// New text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// New status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TodoStatus>,
    /// New reason (for `blocked` or `skipped`); `""` clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Params of `todo.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TodoRemoveParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The entry.
    pub todo_id: TodoId,
}

/// Params of `todo.reorder`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TodoReorderParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The entry to move.
    pub todo_id: TodoId,
    /// Its new 0-based index, clamped to the list length.
    pub to_index: u32,
}

/// Params of `job.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobListParams {
    /// The session.
    pub id: SessionId,
}

/// Result of `job.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobListResult {
    /// Every job of the session, newest first.
    pub jobs: Vec<Job>,
}

/// Params of `job.logs`: read a range of a job's log (for humans; agents read the file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobLogsParams {
    /// The session.
    pub id: SessionId,
    /// The job.
    pub job_id: JobId,
    /// The first line to return, from 1; default: the last `max_lines` lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_line: Option<u64>,
    /// At most this many lines; default and cap chosen by the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lines: Option<u32>,
}

/// Result of `job.logs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobLogs {
    /// The number of the first returned line, from 1.
    pub from_line: u64,
    /// The lines, ANSI-stripped, without newlines.
    pub lines: Vec<String>,
    /// Lines in the log so far.
    pub total_lines: u64,
}

/// Params of commands that only name a job: `job.kill`, `job.unfollow`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobOpParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The job.
    pub job_id: JobId,
}

/// Which output a follow covers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FollowFrom {
    /// All output the agent hasn't been told about (the default), so following right after
    /// starting the job can't miss lines.
    #[default]
    Unseen,
    /// Only output from now on.
    Now,
}

/// Params of `job.follow`: wake the agent on new (or matching) output. Calling it again updates
/// the settings; following ends when the job exits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobFollowParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The job.
    pub job_id: JobId,
    /// The batching window; default 15.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_seconds: Option<u32>,
    /// Only wake on lines matching this regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Which output to cover; default `unseen`.
    #[serde(default)]
    pub from: FollowFrom,
}

/// Params of `sandbox.revoke`: remove a session's sandbox grant. A standing grant restarts the
/// agent (resumed) so the profile change applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SandboxRevokeParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The grant.
    pub grant_id: GrantId,
}
