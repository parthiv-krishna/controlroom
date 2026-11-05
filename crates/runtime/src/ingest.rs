//! Folding transcript records into a session.
//!
//! Each driver event is first *observed* (in-memory state the fold implies, rebuilt by every
//! re-fold) and then, for records after the session's `applied_seq`, *reacted* to: persisted
//! consequences such as removing a delivered queue item or pausing the queue after a failed run.
//! A record whose reactions changed the persisted state moves `applied_seq` in the same
//! transaction, so a re-fold after a restart never applies a reaction twice.

use controlroom_drivers::{DriverEvent, LineTag};
use controlroom_protocol::RpcError;
use controlroom_protocol::Seq;
use controlroom_protocol::ids::{QueueId, SessionId, UnixMillis};
use controlroom_protocol::session::{RequestState, RunOutcome};
use controlroom_transcript::{Body, Record};

use crate::core::Core;
use crate::errors::internal;
use crate::inject::is_injection;
use crate::model::{Delivery, QueueEntry, SessionData, Via};
use crate::session::{Ended, Session, SessionUpdate, raw_frame};

/// Records since the last timeline checkpoint before a new one is due.
const CHECKPOINT_RECORDS: u64 = 256;

/// Where a record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The transcript file (a re-fold or a catch-up after the supervisor went away).
    File,
    /// The read stream of this attachment generation.
    Stream(u64),
}

impl Core {
    /// Folds one record into the session (inside a transaction).
    pub(crate) fn ingest(
        &mut self,
        id: SessionId,
        source: Source,
        record: &Record,
    ) -> Result<(), RpcError> {
        let Some(session) = self.sessions.get(&id) else {
            return Ok(());
        };
        if let Source::Stream(generation) = source
            && session.live.attachment.as_ref().map(|a| a.generation) != Some(generation)
        {
            return Ok(());
        }
        let seq = Seq(record.seq);
        if seq <= session.live.processed_seq {
            return Ok(());
        }
        let loading = session.live.loading;
        let session = self.session_mut(id)?;
        let before = session.data.clone();
        let mut out = session.fold(record);
        crate::triggers::enrich(session, &mut out);
        if let Body::Meta(_) = &record.body {
            session.derived.process = crate::session::Process {
                meta_seq: Some(seq),
                live: true,
                ..Default::default()
            };
            session.live.written.clear();
            session.live.handed.clear();
            session.live.cancel_sent = false;
            session.live.launch_error = None;
        }
        if !matches!(record.body, Body::Meta(_) | Body::Exit(_)) && session.view().ready {
            session.derived.process.ready = true;
        }
        for op in &out.ops {
            session.derived.preview.apply(op);
        }
        let finished = finished_tool_ids(&out.ops);
        for event in &out.events {
            observe(session, event);
        }
        if seq > session.data.applied_seq {
            for event in &out.events {
                self.react(id, seq, record, event);
            }
        }
        let Some(session) = self.sessions.get_mut(&id) else {
            return Ok(());
        };
        session.live.processed_seq = seq;
        if seq > session.data.applied_seq && session.data != before {
            session.data.applied_seq = seq;
        }
        if seq > session.data.timeline_seq {
            for op in &out.ops {
                session
                    .timeline
                    .apply(&self.store, id, op, seq)
                    .map_err(internal)?;
            }
            let due = session.timeline.full()
                || seq.0 - session.data.timeline_seq.0 >= CHECKPOINT_RECORDS
                || out
                    .events
                    .iter()
                    .any(|e| matches!(e, DriverEvent::RunSettled { .. }));
            if due {
                self.checkpoint(id);
            }
            self.publish(
                id,
                SessionUpdate {
                    head_seq: seq,
                    ops: out.ops,
                    raw: Some(raw_frame(record)),
                    detail: None,
                },
            );
        }
        if !loading {
            self.resolve_doubt(id);
            self.reconcile(id);
        }
        for tool_call in finished {
            self.tool_finished(id, &tool_call);
        }
        Ok(())
    }

    /// Persisted consequences of an event.
    fn react(&mut self, id: SessionId, seq: Seq, record: &Record, event: &DriverEvent) {
        let ts = UnixMillis(i64::try_from(record.ts).unwrap_or(i64::MAX));
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        let data = &mut session.data;
        match event {
            DriverEvent::Written { tag } if is_injection(tag) => self.injection_written(id, tag),
            DriverEvent::Accepted { tag } if is_injection(tag) => self.injection_accepted(id, tag),
            DriverEvent::Rejected { tag, error } if is_injection(tag) => {
                self.injection_rejected(id, tag, error);
            }
            // The transcript is the truth: the item reached the agent.
            DriverEvent::Written { tag } => {
                if let Some(entry) = data.queue.iter_mut().find(|e| e.has_tag(tag)) {
                    let via = entry.delivery.map_or(Via::Prompt, Delivery::via);
                    entry.delivery = Some(Delivery::Written(via));
                }
            }
            DriverEvent::Accepted { tag } => data.queue.retain(|e| !e.has_tag(tag)),
            DriverEvent::Rejected { tag, error } => {
                let Some(pos) = data.queue.iter().position(|e| e.has_tag(tag)) else {
                    return;
                };
                tracing::info!("session {id}: the agent refused a queued prompt: {error}");
                let entry = data.queue.remove(pos);
                requeue(data, entry, QueueId::generate());
            }
            DriverEvent::RunSettled { outcome, error } => {
                data.last_outcome = Some(*outcome);
                data.last_error.clone_from(error);
                data.last_turn_end_seq = seq;
                crate::auto::run_settled(data);
                if *outcome != RunOutcome::Completed {
                    pause_after_bad_end(data);
                }
            }
            DriverEvent::NativeSession(native) => {
                let started_by = data.pending_start.take();
                let same = data
                    .agent_sessions
                    .last()
                    .is_some_and(|row| row.session.native_id == native.id);
                if started_by.is_none() && same {
                    return;
                }
                if let Some(row) = data.current_agent_session()
                    && row.session.ended_at.is_none()
                {
                    row.session.ended_at = Some(ts);
                }
                data.agent_sessions.push(crate::model::AgentSessionRow {
                    session: controlroom_protocol::session::AgentSession {
                        driver: data.driver,
                        native_id: native.id.clone(),
                        started_at: ts,
                        ended_at: None,
                        started_by: started_by
                            .unwrap_or(controlroom_protocol::session::StartedBy::New),
                    },
                    native_path: native.path.clone(),
                });
            }
            DriverEvent::Exited { during_run, .. } => {
                let ready = session.derived.process.ready;
                end_process(data, seq, ts, *during_run, ready, &exit_error(event));
            }
            DriverEvent::RunStarted { cause } => crate::auto::run_started(data, cause.as_ref()),
            DriverEvent::Ready
            | DriverEvent::RequestOpened(_)
            | DriverEvent::RequestClosed { .. }
            | DriverEvent::Context(_)
            | DriverEvent::Usage { .. }
            | DriverEvent::Options(_)
            | DriverEvent::Title(_) => {}
        }
    }

    /// The agent process is gone without an `exit` record (its supervisor died): the run, if
    /// any, ends at the last record. So does a run a logged prompt was about to start: the
    /// prompt counts as delivered (it is never sent twice), so the user must learn it was cut
    /// off.
    pub(crate) fn agent_gone(&mut self, id: SessionId) -> Result<(), RpcError> {
        let session = self.session_mut(id)?;
        if !session.derived.process.live {
            return Ok(());
        }
        let view = session.view();
        let during_run = view.run_active || view.run_starting;
        let seq = session.live.processed_seq;
        let ts = session.view().last_ts.unwrap_or_default();
        let ready = session.derived.process.ready;
        let error = "the agent's supervisor stopped unexpectedly".to_owned();
        session.derived.process.live = false;
        session.derived.process.ended = Some(Ended {
            during_run,
            before_ready: !ready,
            error: error.clone(),
        });
        session.derived.preview.clear_tools();
        // Guarded so a restart that finds the same dead supervisor doesn't repeat it.
        if session.data.last_turn_end_seq < seq || !during_run {
            end_process(&mut session.data, seq, ts, during_run, ready, &error);
        }
        Ok(())
    }
}

/// In-memory consequences of an event.
fn observe(session: &mut Session, event: &DriverEvent) {
    let derived = &mut session.derived;
    let live = &mut session.live;
    match event {
        DriverEvent::Written { tag } => {
            live.handed.remove(tag);
            if matches!(tag, LineTag::Internal { .. })
                || session
                    .data
                    .decisions
                    .values()
                    .any(|d| d.answer_tag().as_ref() == Some(tag))
            {
                live.written.insert(tag.clone());
            }
            if is_send_now_cancel(tag) {
                live.cancel_sent = true;
            }
        }
        DriverEvent::RequestOpened(request) => {
            derived
                .requests
                .insert(request.id.clone(), (**request).clone());
        }
        DriverEvent::RequestClosed { id } => {
            if let Some(request) = derived.requests.get_mut(id)
                && request.state == RequestState::Pending
            {
                request.state = RequestState::Withdrawn;
            }
        }
        DriverEvent::RunStarted { .. } => live.cancel_sent = false,
        DriverEvent::RunSettled { .. } => {
            live.cancel_sent = false;
            live.written.retain(|t| !is_send_now_cancel(t));
        }
        DriverEvent::Context(context) => derived.context = Some(*context),
        DriverEvent::Usage {
            tokens,
            cost,
            reply,
        } => crate::usage::add(&mut derived.usage, tokens, *cost, *reply),
        DriverEvent::Title(title) => derived.title = Some(title.clone()),
        DriverEvent::Exited { during_run, .. } => {
            derived.process.live = false;
            derived.process.ended = Some(Ended {
                during_run: *during_run,
                before_ready: !derived.process.ready,
                error: exit_error(event),
            });
            derived.preview.clear_tools();
            live.cancel_sent = false;
            live.handed.clear();
        }
        DriverEvent::Ready
        | DriverEvent::NativeSession(_)
        | DriverEvent::Accepted { .. }
        | DriverEvent::Rejected { .. }
        | DriverEvent::Options(_) => {}
    }
}

/// Whether `tag` is a cancel the runtime sent for a send-now (see `reconcile`).
pub(crate) fn is_send_now_cancel(tag: &LineTag) -> bool {
    matches!(tag, LineTag::Internal { key, .. } if key.starts_with(crate::reconcile::SEND_NOW_CANCEL))
}

fn exit_error(event: &DriverEvent) -> String {
    match event {
        DriverEvent::Exited {
            code: Some(code), ..
        } => format!("the agent exited with code {code}"),
        DriverEvent::Exited {
            signal: Some(signal),
            ..
        } => format!("the agent was killed by signal {signal}"),
        _ => "the agent exited".to_owned(),
    }
}

/// Bookkeeping when the agent process ends.
fn end_process(
    data: &mut SessionData,
    seq: Seq,
    ts: UnixMillis,
    during_run: bool,
    ready: bool,
    error: &str,
) {
    if let Some(row) = data.current_agent_session()
        && row.session.ended_at.is_none()
    {
        row.session.ended_at = Some(ts);
    }
    // Lines written but never answered went to a process that is gone: they count as
    // delivered, and are never sent again.
    data.queue
        .retain(|e| !matches!(e.delivery, Some(Delivery::Written(_))));
    data.injections
        .retain(|i| !matches!(i.delivery, Some(Delivery::Written(_))));
    if !ready {
        if data.pending_start == Some(controlroom_protocol::session::StartedBy::Resume) {
            data.resume_failed = true;
        }
        data.pending_start = None;
    }
    if during_run {
        // The run never settled: it has no outcome, only the error.
        data.last_outcome = None;
        data.last_turn_end_seq = seq;
        data.last_error = Some(error.to_owned());
        pause_after_bad_end(data);
    }
}

/// A run ended badly (failed, cancelled, interrupted, ...): the queue stops unless a send-now
/// item is next.
fn pause_after_bad_end(data: &mut SessionData) {
    let head_waits = data
        .waiting()
        .next()
        .is_some_and(|(_, head)| !head.send_now);
    if head_waits {
        data.queue_paused = true;
    }
}

/// Puts a refused item back at the front under a new id (its tag is used up). A refused steer
/// goes next as a prompt; a refused prompt pauses the queue.
fn requeue(data: &mut SessionData, entry: QueueEntry, id: QueueId) {
    let steered = entry.delivery.map(Delivery::via) == Some(Via::Steer);
    let front = data.front();
    data.queue.insert(
        front,
        QueueEntry {
            id,
            direct: false,
            delivery: None,
            no_steer: true,
            send_now: steered && entry.send_now,
            ..entry
        },
    );
    if !steered {
        data.queue_paused = true;
    }
}

/// Tool calls whose end these ops record (their host results can be released).
fn finished_tool_ids(ops: &[controlroom_protocol::timeline::TimelineOp]) -> Vec<String> {
    use controlroom_protocol::timeline::{ItemBody, TimelineOp, ToolStatus};
    ops.iter()
        .filter_map(|op| match op {
            TimelineOp::Upsert { item } => match &item.body {
                ItemBody::ToolCall {
                    tool_call_id,
                    status: ToolStatus::Completed | ToolStatus::Failed | ToolStatus::Cancelled,
                    ..
                } => Some(tool_call_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}
