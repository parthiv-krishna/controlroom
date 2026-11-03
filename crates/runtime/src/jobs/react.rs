//! Folding job transcript records, ending jobs, and delivering wakes (under the runtime's lock).

use std::sync::Arc;

use controlroom_protocol::RpcError;
use controlroom_protocol::ids::{JobId, SessionId, UnixMillis};
use controlroom_protocol::session::JobStatus;
use controlroom_transcript::Record;

use super::JobLive;
use super::model::{JobEnd, KilledBy};
use super::progress::{Folded, Matcher};
use super::wake::{self, WakeEntry, WakeKind};
use crate::core::{Core, Task};
use crate::ports::{JobRef, SupervisorConn};

impl Core {
    /// Starts tracking a running job (at launch or recovery), with its follow subscription.
    /// Returns the seq to read from next, or `None` if the job is unknown or ended.
    pub(crate) fn job_tracked(&mut self, job: JobRef) -> Option<u64> {
        let record = self.sessions.get(&job.session)?.data.jobs.get(&job.job)?;
        if !record.running() {
            return None;
        }
        let follow = record.follow.clone();
        let live = self
            .jobs
            .entry(job.job)
            .or_insert_with(|| JobLive::new(job.session));
        if let Some(follow) = follow {
            let matcher = match follow.pattern.as_deref().map(regex::Regex::new) {
                Some(Ok(regex)) => Matcher::Pattern(regex),
                Some(Err(e)) => {
                    tracing::warn!("job {}: dropping its follow pattern: {e}", job.job);
                    Matcher::Any
                }
                None => Matcher::Any,
            };
            live.progress.follow(matcher);
        }
        Some(live.progress.seq + 1)
    }

    /// Attaches the job's supervisor (if it runs): kills go through `conn`, and one asked for
    /// earlier is sent now. Returns whether the job is tracked; an untracked job (its session
    /// was deleted meanwhile) is stopped.
    pub(crate) fn job_attach(
        &mut self,
        job: JobRef,
        conn: Option<Arc<dyn SupervisorConn>>,
    ) -> Result<bool, RpcError> {
        let tracked = self.job_tracked(job).is_some();
        let Some(conn) = conn else {
            return Ok(tracked);
        };
        let kill = !tracked
            || self
                .sessions
                .get(&job.session)
                .and_then(|s| s.data.jobs.get(&job.job))
                .is_some_and(|r| r.kill.is_some());
        if let Some(live) = self.jobs.get_mut(&job.job) {
            live.conn = Some(conn.clone());
        }
        if kill {
            self.task(Task::Terminate(conn, std::time::Duration::ZERO));
        }
        Ok(tracked)
    }

    /// Folds one record of the job's transcript.
    pub(crate) fn job_record(&mut self, job: JobRef, record: &Record) -> Result<(), RpcError> {
        let Some(baseline) = self
            .sessions
            .get(&job.session)
            .and_then(|s| s.data.jobs.get(&job.job))
            .filter(|r| r.running())
            .map(|r| r.follow_baseline())
        else {
            return Ok(());
        };
        let Some(live) = self.jobs.get_mut(&job.job) else {
            return Ok(());
        };
        match live.progress.fold(record, baseline) {
            Folded::Nothing => Ok(()),
            Folded::Followed => {
                self.schedule_follow_wake(job.session, &job.job);
                Ok(())
            }
            Folded::Exited => self.job_ended(job, None),
        }
    }

    /// The supervisor is gone and its transcript was read to the end: a job that didn't exit
    /// there was lost (its supervisor was killed, or the machine rebooted).
    pub(crate) fn job_detached(&mut self, job: JobRef) -> Result<(), RpcError> {
        let running = self
            .sessions
            .get(&job.session)
            .and_then(|s| s.data.jobs.get(&job.job))
            .is_some_and(|r| r.running());
        if !running {
            self.jobs.remove(&job.job);
            return Ok(());
        }
        self.jobs
            .entry(job.job)
            .or_insert_with(|| JobLive::new(job.session));
        self.job_ended(job, Some("its supervisor stopped unexpectedly"))
    }

    /// Records the job's end (from its exit record, or `lost`), publishes it and wakes the
    /// agent.
    fn job_ended(&mut self, job: JobRef, lost: Option<&str>) -> Result<(), RpcError> {
        let now = self.clock.now();
        let Some(live) = self.jobs.get(&job.job) else {
            return Ok(());
        };
        let progress = &live.progress;
        let end = match (&progress.exited, lost) {
            (Some(exited), _) => JobEnd::from_exit(&exited.info, exited.at, progress.lines),
            (None, error) => JobEnd {
                status: JobStatus::Failed,
                exit: None,
                signal: None,
                ended_at: now,
                lines: progress.lines,
                error: error.map(str::to_owned),
            },
        };
        let record = self.job_record_mut(job.session, &job.job)?;
        record.exit_woken = record.kill == Some(KilledBy::Agent);
        record.end = Some(end);
        self.flush_job_wakes(job.session, true)?;
        let record = self.job_record_mut(job.session, &job.job)?;
        record.follow = None;
        self.jobs.remove(&job.job);
        self.publish_job(job.session, &job.job)
    }

    /// Sets when the job's buffered follow wake fires, if lines wait and none is set.
    pub(crate) fn schedule_follow_wake(&mut self, id: SessionId, job: &JobId) {
        let now = self.clock.now();
        let Some(record) = self.sessions.get(&id).and_then(|s| s.data.jobs.get(job)) else {
            return;
        };
        let (Some(follow), baseline) = (&record.follow, record.follow_baseline()) else {
            return;
        };
        let buffer = i64::from(follow.buffer_seconds) * 1000;
        let Some(live) = self.jobs.get_mut(job) else {
            return;
        };
        if live.progress.due_at.is_none() && live.progress.pending(baseline).is_some() {
            live.progress.due_at = Some(UnixMillis(now.0 + buffer));
            self.touch(id);
        }
    }

    /// Delivers the session's due wakes as one merged wake: every ended job not woken yet and
    /// every followed job with unreported lines, once any of them is due (an exit, or a follow
    /// whose buffer elapsed). `exit_due` says an exit is waiting.
    pub(crate) fn flush_job_wakes(
        &mut self,
        id: SessionId,
        exit_due: bool,
    ) -> Result<(), RpcError> {
        let now = self.clock.now();
        let Some(session) = self.sessions.get(&id) else {
            return Ok(());
        };
        let mut due = exit_due;
        let mut entries = Vec::new();
        for record in session.data.jobs.values() {
            let live = self.jobs.get_mut(&record.id);
            let baseline = record.follow_baseline();
            let kind = match (&record.end, live) {
                (Some(end), live) if !record.exit_woken => {
                    due = true;
                    let pending = live.and_then(|l| l.progress.pending(baseline));
                    WakeKind::Exit {
                        end: end.clone(),
                        duration_ms: end.ended_at.0 - record.started_at.0,
                        pending,
                    }
                }
                (None, Some(live)) if record.follow.is_some() => {
                    let Some(pending) = live.progress.pending(baseline) else {
                        continue;
                    };
                    due |= live.progress.due_at.is_some_and(|at| at <= now);
                    WakeKind::Follow {
                        pending,
                        pattern: record.follow.as_ref().and_then(|f| f.pattern.clone()),
                    }
                }
                _ => continue,
            };
            let lines = match &record.end {
                Some(end) => end.lines,
                None => self.jobs.get(&record.id).map_or(0, |l| l.progress.lines),
            };
            entries.push(WakeEntry {
                job: record.id,
                name: record.name.clone(),
                log_path: record.log_path.clone(),
                lines,
                kind,
            });
        }
        if !due || entries.is_empty() {
            return Ok(());
        }
        let session = self.session_mut(id)?;
        for entry in &entries {
            if let Some(record) = session.data.jobs.get_mut(&entry.job) {
                record.notified_line = record.notified_line.max(entry.lines);
                record.exit_woken |= record.end.is_some();
            }
        }
        for entry in &entries {
            if let Some(live) = self.jobs.get_mut(&entry.job) {
                live.progress.due_at = None;
            }
        }
        if self.session(id)?.data.closed() {
            return Ok(());
        }
        self.deliver_job_wake(id, wake::compose(&entries))
    }

    /// The tick's job work: refreshes shown line counts and fires due follow wakes.
    pub(crate) fn jobs_tick(&mut self) -> Result<(), RpcError> {
        let mut sessions = Vec::new();
        for live in self.jobs.values_mut() {
            if live.progress.shown_lines != live.progress.lines {
                live.progress.shown_lines = live.progress.lines;
                sessions.push(live.session);
            }
        }
        for id in &sessions {
            self.touch(*id);
        }
        let now = self.clock.now();
        let mut due: Vec<SessionId> = self
            .jobs
            .values()
            .filter(|l| l.progress.due_at.is_some_and(|at| at <= now))
            .map(|l| l.session)
            .collect();
        due.dedup();
        for id in due {
            self.flush_job_wakes(id, false)?;
        }
        Ok(())
    }

    /// Hands a wake to the session's injection path.
    fn deliver_job_wake(&mut self, id: SessionId, wake: wake::Wake) -> Result<(), RpcError> {
        tracing::info!("session {id}: {}: {}", wake.label, wake.text);
        Ok(())
    }
}
