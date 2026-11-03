//! Periodic policy, run by the daemon's tick (about once a second): throttled previews,
//! timeline checkpoints, idle-stop, retention, and pruning of the op and rev logs.

use std::time::Duration;

use controlroom_protocol::RpcError;
use controlroom_protocol::ids::{SessionId, UnixMillis};
use controlroom_protocol::session::LiveWork;

use crate::core::{Core, Task};
use crate::errors::internal;

/// Op ids are remembered this long (clients retry within seconds, schedules within minutes).
const OP_RETENTION_MS: i64 = 7 * 24 * 3600 * 1000;
/// Machine-feed changes kept for `machine.subscribe {since_rev}`; older clients get a snapshot.
pub(crate) const REVLOG_KEEP: u64 = 1000;
/// How long an idle-stopped agent gets before SIGKILL.
const IDLE_STOP_GRACE: Duration = Duration::from_secs(5);
const HOUR_MS: i64 = 3600 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;

impl Core {
    pub(crate) fn tick(&mut self) -> Result<(), RpcError> {
        let now = self.clock.now();
        let ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for id in ids {
            let session = &self.sessions[&id];
            if session.emitted.preview_dirty {
                self.emit_session(id, false).map_err(internal)?;
            }
            if !self.sessions[&id].timeline.is_empty() {
                self.checkpoint(id);
            }
            if self.idle_stop_due(id, now) {
                let session = self.session_mut(id)?;
                session.live.stopping = true;
                if let Some(attachment) = &session.live.attachment {
                    let conn = attachment.conn.clone();
                    tracing::info!("session {id}: stopping the idle agent");
                    self.task(Task::Terminate(conn, IDLE_STOP_GRACE));
                }
            }
            if self.retention_due(id, now) {
                tracing::info!("session {id}: purging the closed session (retention)");
                self.delete(id)?;
                self.store.add_tombstone(id, now).map_err(internal)?;
            }
        }
        self.store
            .prune_ops(UnixMillis(now.0 - OP_RETENTION_MS))
            .map_err(internal)?;
        self.store.prune_revs(REVLOG_KEEP).map_err(internal)?;
        Ok(())
    }

    /// Idle-stop (spec §6.6): only a resumable, ready, idle agent with an empty queue, nothing
    /// to inject, no pending approval and no live work, idle for the configured hours (0
    /// disables it).
    fn idle_stop_due(&self, id: SessionId, now: UnixMillis) -> bool {
        let hours = i64::from(self.settings.idle_timeout_h);
        let session = &self.sessions[&id];
        let view = session.view();
        hours > 0
            && session.writable()
            && !session.live.stopping
            && view.ready
            && !session.busy()
            && session.data.queue.is_empty()
            && session.data.injections.is_empty()
            // An open question doesn't keep the agent up: its answer resumes it.
            && !session.pending_requests().iter().any(|r| r.blocking)
            && session.live_work() == LiveWork::default()
            && session.resumable()
            && view
                .last_ts
                .is_some_and(|last| now.0 - last.0 >= hours * HOUR_MS)
    }

    /// Retention: closed sessions are purged after the configured days (default: never).
    fn retention_due(&self, id: SessionId, now: UnixMillis) -> bool {
        let Some(days) = self.settings.retention.delete_closed_after_days else {
            return false;
        };
        self.sessions[&id]
            .data
            .closed_at
            .is_some_and(|at| now.0 - at.0 >= i64::from(days) * DAY_MS)
    }
}
