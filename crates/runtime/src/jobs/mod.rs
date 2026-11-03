//! Background jobs (spec §8 "Job"): supervised processes (supervisor kind `job`) the agent
//! starts with `job run`, under the session's sandbox, with an optional timeout.
//!
//! - **Output.** The job's supervisor logs its output to the job's transcript and appends a
//!   plain-text copy (ANSI-stripped, carriage returns collapsed) to
//!   `<session cwd>/.controlroom/jobs/<id>.log`, which the agent reads in its sandbox. Logs are
//!   never put into the agent's context.
//! - **Wakes.** The exit wake is always on (unless the agent killed the job itself). A follow
//!   subscription (`job.follow {buffer_seconds, pattern?, from}`) also wakes the agent on new or
//!   matching lines, batched for `buffer_seconds`. Wakes of several jobs of a session that are
//!   due together are merged into one; an exit flushes the session's buffered follow wakes with
//!   it. A wake carries job ids, outcomes, line counts and ranges and the log path.
//! - **Exactly once.** A wake is recorded (`notified_line`, `exit_woken`) in the same transaction
//!   that hands it to the session's injection path, so a restart never repeats or loses it.
//! - **Restarts.** Jobs outlive the agent and the daemon. At recovery the daemon re-reads each
//!   running job's transcript (rebuilding [`progress::Progress`]) and reattaches to its
//!   supervisor; a job whose supervisor vanished without an exit record ends as `failed`.
//! - **Live work.** Running jobs count in the summary's `live_work.jobs` and keep idle-stop
//!   away.
//!
//! Modules: `model` (persisted records), `progress` (in-memory output tracking), `wake` (wake
//! text), `commands` (run, kill, follow, unfollow, list), `react` (folding job records, wakes,
//! the tick), `io` (supervisors: launch, readers, rescans, recovery), `api` (the [`Runtime`]
//! methods).
//!
//! [`Runtime`]: crate::Runtime

mod api;
mod commands;
mod io;
mod model;
mod progress;
mod react;
mod wake;

use std::sync::Arc;

use controlroom_protocol::RpcError;
use controlroom_protocol::ids::{ItemId, JobId, Seq, SessionId};
use controlroom_protocol::session::{Job, SessionDetail};
use controlroom_protocol::timeline::{Actor, ItemBody, TimelineItem, TimelineOp};

pub use api::{JobRun, JobStarted};
pub(crate) use io::recover_jobs;
pub(crate) use model::JobRecord;
pub use model::KilledBy;

use crate::core::Core;
use crate::errors::internal;
use crate::ports::SupervisorConn;
use crate::session::{Session, SessionUpdate};
use progress::Progress;

/// Finished jobs listed in the session detail (all of them are in `job.list`).
const DETAIL_FINISHED: usize = 10;

/// A running job in memory.
pub(crate) struct JobLive {
    pub session: SessionId,
    pub progress: Progress,
    /// The supervisor connection, once attached (kills go through it).
    pub conn: Option<Arc<dyn SupervisorConn>>,
    /// Bumped by every follow change, so a stale rescan is dropped.
    pub follow_generation: u64,
}

impl JobLive {
    pub(crate) fn new(session: SessionId) -> Self {
        JobLive {
            session,
            progress: Progress::default(),
            conn: None,
            follow_generation: 0,
        }
    }
}

impl Core {
    /// The session's jobs as the wire shows them, newest first.
    pub(crate) fn job_views(&self, session: &Session) -> Vec<Job> {
        session
            .data
            .jobs
            .values()
            .rev()
            .map(|record| record.to_job(self.jobs.get(&record.id).map(|j| &j.progress)))
            .collect()
    }

    /// The session detail with its jobs: running ones and the latest finished ones.
    pub(crate) fn detail(&self, session: &Session) -> SessionDetail {
        let mut detail = session.detail();
        let mut finished = 0;
        detail.jobs = self
            .job_views(session)
            .into_iter()
            .filter(|job| {
                job.ended_at.is_none() || {
                    finished += 1;
                    finished <= DETAIL_FINISHED
                }
            })
            .collect();
        detail
    }

    /// Writes the job's timeline item and sends it to the session's subscribers. Like a raised
    /// request, it has no transcript seq: it sits at the head when the job started and every
    /// change is stored as changed after the current head, so a resubscribe from the head
    /// still gets it.
    pub(crate) fn publish_job(&mut self, id: SessionId, job: &JobId) -> Result<(), RpcError> {
        let Some(record) = self.sessions.get(&id).and_then(|s| s.data.jobs.get(job)) else {
            return Ok(());
        };
        let view = record.to_job(self.jobs.get(job).map(|j| &j.progress));
        let item = TimelineItem {
            id: job_item_id(job),
            seq_from: record.seq,
            seq_to: record.seq,
            actor: Actor::Root,
            ts: record.started_at,
            streaming: false,
            body: ItemBody::Job { job: view },
        };
        let head = self.head(id);
        self.store
            .put_timeline_item(id, &item, Seq(head.0 + 1))
            .map_err(internal)?;
        self.publish(
            id,
            SessionUpdate {
                head_seq: head,
                ops: vec![TimelineOp::Upsert {
                    item: Box::new(item),
                }],
                raw: None,
                detail: None,
            },
        );
        Ok(())
    }
}

/// The timeline item of a job.
pub(crate) fn job_item_id(job: &JobId) -> ItemId {
    ItemId::new(format!("job-{job}"))
}
