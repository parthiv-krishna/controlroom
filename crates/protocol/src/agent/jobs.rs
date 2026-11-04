//! The agent's `job` tool (spec §9): `run`, `follow`, `unfollow`, `status`, `kill`. There is no
//! logs action: the agent reads the job's log file in its sandbox.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::daemon::FollowFrom;
use crate::ids::{JobId, RemotePath};
use crate::session::Job;

/// Params of `job.run`: start a background job under the session's sandbox (with remote tools:
/// on a workspace's host, under that workspace's sandbox).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobRunParams {
    /// Idempotency key: the pi tool call id.
    pub call_id: String,
    /// The command line, run by the login shell.
    pub command: String,
    /// With remote tools: the machine's display name (one of the session's workspaces);
    /// default the primary workspace's. The old daemon ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// The working directory: absolute or relative to the (machine's primary) workspace root,
    /// and inside it (the old daemon: relative to the session's cwd).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<RemotePath>,
    /// Stop the job after this many seconds (it ends `timed_out`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u32>,
    /// A short display name; default: the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Result of `job.run`. Its exit always wakes the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobRunResult {
    /// The job.
    pub job_id: JobId,
    /// Its plain-text log (ANSI-stripped), for the agent to read with tail, grep or sed.
    pub log_path: RemotePath,
}

/// Params of `job.follow`: wake the agent on new (or matching) lines. Calling it again updates
/// the settings; following ends when the job exits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentJobFollowParams {
    /// Idempotency key: the pi tool call id.
    pub call_id: String,
    /// The job.
    pub job_id: JobId,
    /// The batching window; default 15.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_seconds: Option<u32>,
    /// Only wake on lines matching this regex (e.g. `ERROR|FAIL|panicked`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Which output to cover; default `unseen` (everything no wake reported yet).
    #[serde(default)]
    #[ts(optional, as = "Option<FollowFrom>")]
    pub from: FollowFrom,
}

/// Params of `job.unfollow` and `job.kill`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentJobParams {
    /// Idempotency key: the pi tool call id.
    pub call_id: String,
    /// The job.
    pub job_id: JobId,
}

/// Params of `job.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobStatusParams {
    /// One job; default: every job of the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
}

/// Result of `job.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentJobs {
    /// The jobs, newest first.
    pub jobs: Vec<Job>,
}
