//! Folding a host's job event log into the sessions' jobs, and deciding wakes (design §3.6).
//!
//! **Exactly once.** A batch of events (a subscription's backlog, or a `jobs.event`) is applied
//! in one transaction together with the host's new cursor and the injections the events cause.
//! Events at or below the cursor are skipped, and every wake has a deterministic id
//! (`exit:<machine>/<job>`, `follow:<machine>/<job>:<first>-<last>`) recorded with the job, so a
//! replay can't wake the agent twice.
//!
//! **Wakes.** A job's exit always wakes the agent, unless the agent killed it. A followed job's
//! matching lines accumulate from the first one's host timestamp for `buffer_seconds`, then
//! wake it with their line ranges. When one wake of a session is due, every job of the session
//! with unreported lines comes along, and all of them go into one injection (origin `job`); an
//! exit carries its job's unreported lines. After an outage, the backlog therefore produces one
//! merged wake per session for everything that happened meanwhile.
//!
//! **Unreachable hosts.** Nothing is invented while a host is away: its jobs stay `running` (and
//! count as live work). Once the machine's grace period has passed, each session with running
//! jobs there gets one notice per outage (not waking the agent).

use std::collections::BTreeSet;

use controlroom_protocol::RpcError;
use controlroom_protocol::host::{JobEvent, JobEventKind, KilledBy};
use controlroom_protocol::ids::{JobId, MachineId, SessionId};
use controlroom_protocol::session::JobStatus;
use controlroom_protocol::timeline::InjectOrigin;

use super::model::{FollowSpec, JobEnd, JobRecord, Pending};
use super::wake::{self, WakeEntry, WakeKind};
use crate::core::Core;
use crate::errors::internal;
use crate::model::Injection;

impl Core {
    /// Applies a batch of `machine`'s job events after its cursor, moves the cursor and
    /// delivers the wakes they make due, all in the caller's transaction.
    pub(crate) fn job_events(
        &mut self,
        machine: MachineId,
        events: Vec<JobEvent>,
    ) -> Result<(), RpcError> {
        let mut cursor = self.store.cursor(machine).map_err(internal)?;
        let mut touched = BTreeSet::new();
        for event in events {
            if cursor.is_some_and(|c| event.rev <= c) {
                continue;
            }
            cursor = Some(event.rev);
            if let Some(id) = self.apply_job_event(machine, &event)? {
                touched.insert((id, event.job_id));
            }
        }
        if let Some(cursor) = cursor {
            self.store.set_cursor(machine, cursor).map_err(internal)?;
        }
        let sessions: BTreeSet<SessionId> = touched.iter().map(|(id, _)| *id).collect();
        for id in sessions {
            self.flush_job_wakes(id)?;
        }
        for (id, job) in touched {
            self.publish_job(id, &job)?;
        }
        Ok(())
    }

    /// Applies one event; returns the job's session, if the job belongs to one.
    fn apply_job_event(
        &mut self,
        machine: MachineId,
        event: &JobEvent,
    ) -> Result<Option<SessionId>, RpcError> {
        let job = event.job_id;
        if let JobEventKind::Started {
            session_id,
            workspace_id,
            name,
            command,
            cwd,
            log_path,
            timeout_s,
            follow,
        } = &event.kind
        {
            let head = self.head(*session_id);
            let Ok(session) = self.session_mut(*session_id) else {
                return Ok(None);
            };
            let record = session.data.jobs.entry(job).or_insert_with(|| JobRecord {
                id: job,
                machine_id: machine,
                workspace_id: *workspace_id,
                name: name.clone(),
                command: command.clone(),
                cwd: cwd.clone(),
                log_path: log_path.clone(),
                timeout_s: *timeout_s,
                started_at: event.at,
                seq: head,
                lines: 0,
                follow: None,
                notified_line: 0,
                pending: Pending::default(),
                first_match_at: None,
                kill: None,
                end: None,
                exit_woken: false,
                wakes: Vec::new(),
            });
            record.started_at = event.at;
            if let Some(follow) = follow {
                record.follow.get_or_insert(FollowSpec {
                    buffer_seconds: follow.buffer_seconds,
                    pattern: follow.pattern.clone(),
                });
            }
            return Ok(Some(*session_id));
        }
        let Some(id) = self.job_session(machine, &job) else {
            return Ok(None);
        };
        let record = self.job_record_mut(id, &job)?;
        match &event.kind {
            JobEventKind::Progress { lines, matches, .. } => {
                record.lines = record.lines.max(*lines);
                if record.follow.is_some() && record.running() {
                    add_matches(record, matches, event.at);
                }
            }
            JobEventKind::Follow {
                pattern,
                buffer_seconds,
                matches,
                ..
            } => {
                record.follow = Some(FollowSpec {
                    buffer_seconds: *buffer_seconds,
                    pattern: pattern.clone(),
                });
                if record.running() {
                    add_matches(record, matches, event.at);
                }
            }
            JobEventKind::Unfollow => {
                record.follow = None;
                record.pending = Pending::default();
                record.first_match_at = None;
            }
            JobEventKind::Killed { by } => {
                // The agent's own kill (recorded when it asked) wins over a later user kill.
                record.kill.get_or_insert(*by);
            }
            JobEventKind::Exit {
                exit_code,
                signal,
                timed_out,
                lost,
                lines,
                duration_ms,
            } => {
                let status = match (lost, timed_out, &record.kill, exit_code) {
                    (Some(_), ..) => JobStatus::Lost,
                    (None, true, ..) => JobStatus::TimedOut,
                    (None, false, _, Some(0)) => JobStatus::Succeeded,
                    (None, false, Some(_), _) => JobStatus::Killed,
                    _ => JobStatus::Failed,
                };
                record.lines = record.lines.max(*lines);
                record.exit_woken |= record.kill == Some(KilledBy::Agent);
                record.end.get_or_insert(JobEnd {
                    status,
                    exit: *exit_code,
                    signal: signal.clone(),
                    ended_at: event.at,
                    lines: *lines,
                    duration_ms: *duration_ms,
                    lost: lost.clone(),
                });
            }
            JobEventKind::Started { .. } | JobEventKind::Unknown => {}
        }
        Ok(Some(id))
    }

    /// The session that has `machine`'s job.
    fn job_session(&self, machine: MachineId, job: &JobId) -> Option<SessionId> {
        self.sessions
            .values()
            .find(|s| {
                s.data
                    .jobs
                    .get(job)
                    .is_some_and(|j| j.machine_id == machine)
            })
            .map(|s| s.data.id)
    }

    /// Delivers the session's due wakes as one merged wake (see the module docs).
    pub(crate) fn flush_job_wakes(&mut self, id: SessionId) -> Result<(), RpcError> {
        let now = self.clock.now();
        let Some(session) = self.sessions.get(&id) else {
            return Ok(());
        };
        let mut due = false;
        let mut entries = Vec::new();
        for record in session.data.jobs.values() {
            let pending = (!record.pending.is_empty()).then(|| record.pending.clone());
            let kind = match &record.end {
                Some(end) if !record.exit_woken => {
                    due = true;
                    WakeKind::Exit {
                        end: end.clone(),
                        duration_ms: end.duration_ms,
                        pending,
                    }
                }
                None if record.follow.is_some() => {
                    let Some(pending) = pending else {
                        continue;
                    };
                    due |= record.follow_due_at().is_some_and(|at| at <= now);
                    WakeKind::Follow {
                        pending,
                        pattern: record.follow.as_ref().and_then(|f| f.pattern.clone()),
                    }
                }
                _ => continue,
            };
            entries.push(WakeEntry {
                job: record.id,
                name: record.name.clone(),
                log_path: record.log_path.clone(),
                machine: session.machines.name(&record.machine_id),
                lines: record.end.as_ref().map_or(record.lines, |e| e.lines),
                kind,
            });
        }
        if !due {
            return Ok(());
        }
        let mut fresh = Vec::new();
        let session = self.session_mut(id)?;
        for entry in entries {
            let Some(record) = session.data.jobs.get_mut(&entry.job) else {
                continue;
            };
            let wake_id = wake_id(record, &entry.kind);
            let new = !record.wakes.contains(&wake_id);
            record.notified_line = record.notified_line.max(record.pending.last().unwrap_or(0));
            record.pending = Pending::default();
            record.first_match_at = None;
            record.exit_woken |= record.end.is_some();
            if new {
                record.woke(wake_id);
                fresh.push(entry);
            }
        }
        if fresh.is_empty() || session.data.closed() {
            return Ok(());
        }
        let wake = wake::compose(&fresh);
        let injection = Injection::new(InjectOrigin::Job, wake.label, wake.text, true, now);
        self.inject(id, injection)
    }

    /// The tick's job work: due follow wakes, and one notice per outage for sessions whose
    /// running jobs wait on an unreachable host.
    pub(crate) fn jobs_tick(&mut self) -> Result<(), RpcError> {
        let now = self.clock.now();
        let machines = self.machines.clone();
        let mut due = Vec::new();
        let mut notices = Vec::new();
        for session in self.sessions.values() {
            let data = &session.data;
            if data
                .jobs
                .values()
                .any(|j| j.running() && j.follow_due_at().is_some_and(|at| at <= now))
            {
                due.push(data.id);
            }
            if data.closed() {
                continue;
            }
            let waiting: BTreeSet<MachineId> = data
                .jobs
                .values()
                .filter(|j| j.running())
                .map(|j| j.machine_id)
                .collect();
            for machine in waiting {
                let Some(since) = machines.down_since(&machine) else {
                    continue;
                };
                if now.0 - since.0 >= machines.grace_ms(&machine)
                    && data.outages.get(&machine) != Some(&since)
                {
                    notices.push((data.id, machine, since));
                }
            }
        }
        for id in due {
            self.flush_job_wakes(id)?;
        }
        for (id, machine, since) in notices {
            self.session_mut(id)?.data.outages.insert(machine, since);
            let name = machines.name(&machine);
            let minutes = (now.0 - since.0) / 60_000;
            let text = format!(
                "[controlroom] {name} has been unreachable for {minutes} minutes; its jobs keep \
                 running there, and their wakes arrive when it is back."
            );
            let injection = Injection::new(
                InjectOrigin::Workspace,
                format!("{name} unreachable"),
                text,
                false,
                now,
            );
            self.inject(id, injection)?;
        }
        Ok(())
    }
}

/// Adds matching lines after the last reported one.
fn add_matches(record: &mut JobRecord, matches: &[u64], at: controlroom_protocol::UnixMillis) {
    for line in matches.iter().filter(|l| **l > record.notified_line) {
        if record.pending.is_empty() {
            record.first_match_at = Some(at);
        }
        record.pending.add(*line);
    }
}

/// The deterministic id of a wake.
fn wake_id(record: &JobRecord, kind: &WakeKind) -> String {
    let job = format!("{}/{}", record.machine_id, record.id);
    match kind {
        WakeKind::Exit { .. } => format!("exit:{job}"),
        WakeKind::Follow { pending, .. } => format!(
            "follow:{job}:{}-{}",
            pending.first().unwrap_or(0),
            pending.last().unwrap_or(0)
        ),
    }
}
