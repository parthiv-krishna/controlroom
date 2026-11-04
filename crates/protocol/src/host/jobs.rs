//! The host's job event log (`jobs.subscribe`, `jobs.event`, `jobs.ack`).
//!
//! The host follows every running job's output and appends events to a persisted log with one
//! revision sequence per host. It never decides wakes and never waits for the server: the log
//! is complete whether or not anyone is connected, and a restarted host daemon replays it to
//! continue scanning where it stopped. The agent daemon keeps a cursor per host, resubscribes
//! from it after every `hello`, and commits the new cursor together with the wakes the events
//! produce, so each wake is delivered exactly once.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::daemon::FollowFrom;
use crate::host::{JobFollowSpec, KilledBy};
use crate::ids::{JobId, RemotePath, Rev, SessionId, SubscriptionId, UnixMillis, WorkspaceId};

/// What happened to a job, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobEventKind {
    /// The job was accepted; logged before its process exists, so a daemon that dies in
    /// between reports `exit {lost}` rather than a phantom running job.
    Started {
        /// Its session.
        session_id: SessionId,
        /// Its workspace.
        workspace_id: WorkspaceId,
        /// Display name.
        name: String,
        /// The command line.
        command: String,
        /// The working directory.
        cwd: RemotePath,
        /// The plain-text log.
        log_path: RemotePath,
        /// Its timeout, if any.
        timeout_s: Option<u32>,
        /// Followed from the start, if requested.
        follow: Option<JobFollowSpec>,
    },
    /// New complete lines were scanned.
    Progress {
        /// Lines in the log so far.
        lines: u64,
        /// Bytes of the log scanned so far.
        offset: u64,
        /// Line numbers (from 1) among the new lines that match the follow (all of them for a
        /// follow without a pattern); empty when not followed.
        matches: Vec<u64>,
    },
    /// A follow subscription started or changed.
    Follow {
        /// Only matching lines count; `null` for every line.
        pattern: Option<String>,
        /// The batching window.
        buffer_seconds: u32,
        /// Which output it covers.
        from: FollowFrom,
        /// With `from: unseen`: the matching line numbers among the lines so far.
        matches: Vec<u64>,
    },
    /// The follow subscription ended.
    Unfollow,
    /// A kill was requested.
    Killed {
        /// Who asked.
        by: KilledBy,
    },
    /// The job ended; logged once its output is fully scanned.
    Exit {
        /// The exit status; `null` for a signal or when lost.
        exit_code: Option<i32>,
        /// The terminating signal name, if any.
        signal: Option<String>,
        /// It ran into its timeout.
        timed_out: bool,
        /// Why the outcome is unknown, e.g. `"host rebooted"` (its supervisor is gone); `null`
        /// for a normal end.
        lost: Option<String>,
        /// Lines in the log.
        lines: u64,
        /// How long it ran.
        duration_ms: u64,
    },
    /// An event this build doesn't know; skip it (its revision still counts).
    #[serde(other)]
    Unknown,
}

/// One entry of the job event log; on the wire the kind's fields sit next to `rev`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobEvent {
    /// Its position in the host's log; strictly increasing, persisted.
    pub rev: Rev,
    /// The host's time when it was logged. Follow buffers are computed from it, so a replay
    /// gives the same wakes.
    pub at: UnixMillis,
    /// The job.
    pub job_id: JobId,
    /// What happened.
    #[serde(flatten)]
    pub kind: JobEventKind,
}

/// Params of `jobs.subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobsSubscribeParams {
    /// The last revision the subscriber consumed; `null` for everything still in the log.
    pub since: Option<Rev>,
}

/// Result of `jobs.subscribe`: the backlog, then `jobs.event` notifications with the events
/// after `rev` (events logged while the backlog was read are not lost: they follow).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobsSubscribeResult {
    /// Identifies this subscription.
    pub subscription_id: SubscriptionId,
    /// The log's current revision.
    pub rev: Rev,
    /// Events after `since`, in order. The oldest available event is later than `since + 1`
    /// only if they were pruned below an ack, which a subscriber never sees.
    pub events: Vec<JobEvent>,
}

/// Params of the `jobs.event` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobsEventParams {
    /// The subscription.
    pub subscription_id: SubscriptionId,
    /// New events, in order.
    pub events: Vec<JobEvent>,
}

/// Params of `jobs.ack`: the subscriber has durably consumed the log up to `rev`. The host
/// prunes events of ended jobs at or below every subscriber's ack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobsAckParams {
    /// The consumed revision.
    pub rev: Rev,
}
