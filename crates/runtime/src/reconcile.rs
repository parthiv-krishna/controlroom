//! The reconcile loop: one place decides what to write to a session's agent next.
//!
//! It runs after every change (record, command, resolved doubt) and writes at most what is due,
//! in this order:
//!
//! 1. nothing at all while the session is in doubt, loading, closed or has no live process;
//! 2. a state query until the agent has answered once (per process), and again after an
//!    in-process start fresh while the driver doesn't know the new native session (pi
//!    names it only when asked);
//! 3. answers for decided requests whose answer isn't in the transcript or on its way;
//! 4. a requested cancel;
//! 5. the queue: while the agent is busy, a send-now item is steered in (driver capability) or
//!    the run is cancelled at a safe point (no open tool, a pending request, or "interrupt
//!    now"); while it is idle, the head item is sent if it is a send-now item or the queue
//!    auto-sends and isn't paused.
//! 6. injections (`inject`): steered into an active run, or a waking one as a prompt;
//! 7. auto-continue (`auto`), once the agent is idle after a run settled.
//!
//! Every line carries a [`LineTag`]; a tag that was handed over or seen in the transcript is
//! never sent again.

use controlroom_drivers::{Intent, LineTag};
use controlroom_protocol::ids::SessionId;
use controlroom_protocol::session::{ContentPart, RequestState, StartedBy};

use crate::core::Core;
use crate::model::{Decision, Delivery, Via};
use crate::session::Doubt;

/// Key prefix of the cancel a send-now item causes: `cancel:<queue id>`.
pub(crate) const SEND_NOW_CANCEL: &str = "cancel:";
/// Key prefix of the state query after a start, `refresh:<meta seq>`, and after an in-process
/// start fresh, `refresh:<meta seq>:<agent sessions so far>`.
const REFRESH: &str = "refresh:";

impl Core {
    /// Writes what the session needs next (see the module docs).
    pub(crate) fn reconcile(&mut self, id: SessionId) {
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        if !session.writable() {
            return;
        }
        if !session.view().ready {
            let meta = session.derived.process.meta_seq.map_or(0, |s| s.0);
            let tag = internal_tag(format!("{REFRESH}{meta}"));
            if !session.live.sent(&tag) {
                self.write(id, &Intent::Refresh, tag);
            }
            return;
        }
        if session.data.pending_start == Some(StartedBy::Fresh) && session.view().native.is_none() {
            let meta = session.derived.process.meta_seq.map_or(0, |s| s.0);
            let count = session.data.agent_sessions.len();
            let tag = internal_tag(format!("{REFRESH}{meta}:{count}"));
            if !session.live.sent(&tag) {
                self.write(id, &Intent::Refresh, tag);
            }
        }
        self.write_answers(id);
        self.write_cancel(id);
        self.advance_queue(id);
        self.deliver_injections(id);
        self.auto_continue(id);
    }

    /// Encodes and stages `intent`; returns whether it was staged.
    fn write(&mut self, id: SessionId, intent: &Intent, tag: LineTag) -> bool {
        let Ok(session) = self.session_mut(id) else {
            return false;
        };
        match session.driver_mut().encode(intent, &tag) {
            Ok(lines) => {
                session.live.handed.insert(tag);
                for line in lines {
                    self.stage_line(id, line);
                }
                true
            }
            Err(e) => {
                tracing::warn!("session {id}: can't encode {intent:?}: {e}");
                false
            }
        }
    }

    fn write_answers(&mut self, id: SessionId) {
        let session = &self.sessions[&id];
        let open = &session.view().open_requests;
        let due: Vec<_> = session
            .data
            .decisions
            .values()
            .filter(|d| open.contains(&d.request.id))
            .filter_map(|d| {
                let tag = d.answer_tag()?;
                let response = d.request.answer.as_ref()?.response.clone();
                (!session.live.sent(&tag)).then(|| (d.request.id.clone(), response, tag))
            })
            .collect();
        for (request, response, tag) in due {
            self.write(id, &Intent::Answer { request, response }, tag);
        }
    }

    fn write_cancel(&mut self, id: SessionId) {
        let session = &self.sessions[&id];
        let Some(op_id) = session.live.cancel_requested.clone() else {
            return;
        };
        let busy = session.busy();
        if let Ok(session) = self.session_mut(id) {
            session.live.cancel_requested = None;
        }
        if busy && self.write(id, &Intent::Cancel, LineTag::Op { op_id, part: 0 }) {
            self.cancelled(id);
        }
    }

    fn advance_queue(&mut self, id: SessionId) {
        let session = &self.sessions[&id];
        let Some((index, head)) = session.data.waiting().next() else {
            return;
        };
        let head = head.clone();
        let view = session.view();
        if session.busy() {
            if !head.send_now || !view.run_active {
                return;
            }
            if view.capabilities.steer && !head.no_steer {
                let intent = Intent::Steer {
                    content: head.content.clone(),
                };
                let delivered = self.write(id, &intent, head.tag());
                if let Ok(session) = self.session_mut(id) {
                    if delivered {
                        // It starts the next run if this one settles before the agent
                        // reads it.
                        crate::triggers::record_steered_entry(&mut session.data, &head);
                        session.data.queue[index].delivery = Some(Delivery::Handed(Via::Steer));
                    } else {
                        session.data.queue[index].no_steer = true;
                    }
                }
                return;
            }
            let safe_point = view.open_tools == 0
                || !view.open_requests.is_empty()
                || session.pending_raised().any(|r| r.blocking)
                || head.interrupt;
            let tag = internal_tag(format!("{SEND_NOW_CANCEL}{}", head.id));
            if safe_point
                && !session.live.cancel_sent
                && !session.live.sent(&tag)
                && self.write(id, &Intent::Cancel, tag)
            {
                self.cancelled(id);
            }
            return;
        }
        let data = &session.data;
        if !head.send_now && (!data.auto_send || data.queue_paused) {
            return;
        }
        let mut content = head.content.clone();
        if let Some(path) = &data.history_note {
            let note = format!(
                "Earlier conversation from this session is in `{path}`; read it if relevant."
            );
            content.insert(0, ContentPart::text(note));
        }
        let delivered = self.write(id, &Intent::Prompt { content }, head.tag());
        if let Ok(session) = self.session_mut(id) {
            if delivered {
                crate::triggers::record_queue_entry(&mut session.data, &head);
                session.data.queue[index].delivery = Some(Delivery::Handed(Via::Prompt));
                session.data.history_note = None;
            } else {
                // Don't retry an item the driver can't send; the user decides.
                session.data.queue[index].send_now = false;
                session.data.queue_paused = true;
            }
        }
    }

    /// A cancel was handed over: the run's undecided requests become moot, so a late answer
    /// learns that instead of being sent. Raised requests are withdrawn (their waiters deny).
    fn cancelled(&mut self, id: SessionId) {
        if let Err(e) = self.end_pending_raised(id, RequestState::Withdrawn, false) {
            tracing::warn!("session {id}: withdrawing raised requests: {e}");
        }
        let Ok(session) = self.session_mut(id) else {
            return;
        };
        session.live.cancel_sent = true;
        for mut request in session.pending_agent_requests() {
            request.state = RequestState::Withdrawn;
            let decision = Decision {
                request,
                op_id: None,
            };
            session
                .data
                .decisions
                .insert(decision.request.id.clone(), decision);
        }
    }

    /// Ends the doubt once the transcript has been folded through the barrier's durable seq:
    /// what was handed over but isn't in the transcript never reached the agent and may be sent
    /// (again).
    pub(crate) fn resolve_doubt(&mut self, id: SessionId) {
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let Some(Doubt {
            until: Some(until),
            write_failed,
        }) = session.live.doubt
        else {
            return;
        };
        if session.live.processed_seq < until {
            return;
        }
        self.settle_handed(id, write_failed);
    }

    /// Forgets lines handed over but not written: they are known not to have reached the
    /// agent. After a failed write their items wait for the user (the queue pauses).
    pub(crate) fn settle_handed(&mut self, id: SessionId, write_failed: bool) {
        let Ok(session) = self.session_mut(id) else {
            return;
        };
        session.live.doubt = None;
        session.live.handed.clear();
        let mut returned = false;
        for entry in &mut session.data.queue {
            if let Some(Delivery::Handed(_)) = entry.delivery {
                entry.delivery = None;
                returned = true;
                if write_failed {
                    entry.send_now = false;
                    entry.interrupt = false;
                }
            }
        }
        for injection in &mut session.data.injections {
            if let Some(Delivery::Handed(_)) = injection.delivery {
                injection.delivery = None;
            }
        }
        if returned && write_failed {
            session.data.queue_paused = true;
        }
    }
}

fn internal_tag(key: String) -> LineTag {
    LineTag::Internal { key, part: 0 }
}
