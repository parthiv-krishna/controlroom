//! Background jobs (spec §8 "Job", design §3.6): commands the agent starts with `job run` on a
//! workspace's host, where a detached job supervisor runs them under the workspace's sandbox
//! with a plain-text log the agent reads as `machine:path`.
//!
//! The host follows every job and appends what happens to its persisted job event log; the
//! agent daemon keeps a cursor per host, folds the events into the sessions' jobs and decides
//! the wakes itself (`events`). Commands (run, follow, unfollow, kill) go to the host through
//! the call journal, at most once (`commands`). Wakes carry metadata and line ranges, never log
//! content (`wake`). Running jobs count in the summary's `live_work.jobs`: auto-continue waits
//! for them and idle-stop leaves their agent alone.

mod commands;
mod events;
mod model;
mod wake;

use controlroom_protocol::RpcError;
use controlroom_protocol::ids::{ItemId, JobId, Seq, SessionId};
use controlroom_protocol::session::SessionDetail;
use controlroom_protocol::timeline::{Actor, ItemBody, TimelineItem, TimelineOp};

pub(crate) use model::JobRecord;

use crate::core::Core;
use crate::errors::internal;
use crate::session::{Session, SessionUpdate};

/// Finished jobs listed in the session detail (all of them are in `job.list`).
const DETAIL_FINISHED: usize = 10;

impl Core {
    /// The session detail with its jobs: running ones and the latest finished ones.
    pub(crate) fn detail(&self, session: &Session) -> SessionDetail {
        let mut detail = session.detail();
        let mut finished = 0;
        detail.jobs = session
            .data
            .jobs
            .values()
            .rev()
            .map(JobRecord::to_job)
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
    /// request, it has no transcript seq: it sits at the head when the job appeared and every
    /// change is stored as changed after the current head, so a resubscribe from the head
    /// still gets it.
    pub(crate) fn publish_job(&mut self, id: SessionId, job: &JobId) -> Result<(), RpcError> {
        let Some(record) = self.sessions.get(&id).and_then(|s| s.data.jobs.get(job)) else {
            return Ok(());
        };
        let item = TimelineItem {
            id: job_item_id(job),
            seq_from: record.seq,
            seq_to: record.seq,
            actor: Actor::Root,
            ts: record.started_at,
            streaming: false,
            body: ItemBody::Job {
                job: record.to_job(),
            },
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
fn job_item_id(job: &JobId) -> ItemId {
    ItemId::new(format!("job-{job}"))
}
