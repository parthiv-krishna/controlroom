//! Injection (spec §8): the single path that delivers an item into the agent's next turn,
//! with its origin (an answer, a user edit of the todo list, a continuation; later job wakes,
//! child results and messages).
//!
//! The reconcile loop delivers waiting injections after the queue:
//!
//! - while a run is active, each is steered in (when the driver can steer);
//! - while the agent is idle, the first *waking* injection is sent as a prompt and starts a
//!   run whose trigger is its origin; the others follow as steers once that run is active;
//! - non-waking injections (todo notices) wait for the next run. A driver that can't steer
//!   treats every injection as waking, so nothing waits forever.
//!
//! Lines are tagged `inject:<id>`, so delivery follows the same exactly-once rules as the
//! queue: written means delivered, a refused item comes back (and is dropped after a few
//! refusals), lines handed over but not written come back after a doubt.

use controlroom_drivers::{Intent, LineTag};
use controlroom_protocol::RpcError;
use controlroom_protocol::ids::SessionId;

use crate::commands::start_mode_of;
use crate::core::Core;
use crate::model::{Delivery, INJECT, Injection, SessionData, Via};

impl Core {
    /// Queues an injection. A waking one starts a stopped agent (resuming it).
    pub(crate) fn inject(&mut self, id: SessionId, injection: Injection) -> Result<(), RpcError> {
        let wake = injection.wake;
        let session = self.session_mut(id)?;
        session.data.injections.push(injection);
        let session = self.session(id)?;
        if wake && !session.data.closed() && !session.has_process() {
            // Not resumable: it waits until the user starts the agent again.
            if let Ok(by) = start_mode_of(session) {
                self.request_start(id, by)?;
            }
        }
        self.reconcile(id);
        Ok(())
    }

    /// Delivers waiting injections (see the module docs). Called by the reconcile loop.
    pub(crate) fn deliver_injections(&mut self, id: SessionId) {
        let session = &self.sessions[&id];
        let data = &session.data;
        let view = session.view();
        // A prompt on its way starts a run: wait until it is active.
        if prompt_on_its_way(data) || data.injections.iter().all(|i| i.delivery.is_some()) {
            return;
        }
        let steer = view.capabilities.steer;
        if view.run_active {
            if !steer {
                return;
            }
            let due: Vec<usize> = waiting(&data.injections)
                .filter(|(_, i)| !i.no_steer)
                .map(|(index, _)| index)
                .collect();
            for index in due {
                self.send_injection(id, index, Via::Steer);
            }
            return;
        }
        if session.busy() {
            return;
        }
        let first = waiting(&data.injections)
            .find(|(_, i)| i.wake || i.no_steer || !steer)
            .map(|(index, _)| index);
        if let Some(index) = first {
            self.send_injection(id, index, Via::Prompt);
        }
    }

    /// Encodes and stages one injection.
    fn send_injection(&mut self, id: SessionId, index: usize, via: Via) {
        let Ok(session) = self.session_mut(id) else {
            return;
        };
        let injection = session.data.injections[index].clone();
        let intent = Intent::Inject {
            origin: injection.origin,
            content: injection.content.clone(),
        };
        let tag = injection.tag();
        match session.driver_mut().encode(&intent, &tag) {
            Ok(lines) => {
                if via == Via::Prompt {
                    session.data.record_trigger(&tag, injection.trigger());
                }
                session.data.injections[index].delivery = Some(Delivery::Handed(via));
                session.live.handed.insert(tag);
                for line in lines {
                    self.stage_line(id, line);
                }
            }
            Err(e) => {
                tracing::warn!("session {id}: can't deliver an injection: {e}");
                session.data.injections[index].no_steer = true;
            }
        }
    }

    /// The transcript shows an injection's line: it reached the agent.
    pub(crate) fn injection_written(&mut self, id: SessionId, tag: &LineTag) {
        if let Some(injection) = self.injection_mut(id, tag) {
            let via = injection.delivery.map_or(Via::Prompt, Delivery::via);
            injection.delivery = Some(Delivery::Written(via));
        }
    }

    /// The agent took an injection: it is delivered.
    pub(crate) fn injection_accepted(&mut self, id: SessionId, tag: &LineTag) {
        if let Some(session) = self.sessions.get_mut(&id) {
            session.data.injections.retain(|i| i.tag() != *tag);
        }
    }

    /// The agent refused an injection: it waits again under a new id (its tag is used up), as
    /// a prompt if it was a steer; after a few refusals it is dropped.
    pub(crate) fn injection_rejected(&mut self, id: SessionId, tag: &LineTag, error: &str) {
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        let data = &mut session.data;
        let Some(pos) = data.injections.iter().position(|i| i.tag() == *tag) else {
            return;
        };
        let mut injection = data.injections.remove(pos);
        injection.attempts += 1;
        if injection.attempts >= Injection::MAX_ATTEMPTS {
            tracing::warn!("session {id}: dropping an injection the agent refused: {error}");
            return;
        }
        injection.no_steer |= injection.delivery.map(Delivery::via) == Some(Via::Steer);
        injection.delivery = None;
        injection.id = ulid::Ulid::generate().to_string();
        data.injections.insert(pos, injection);
    }

    fn injection_mut(&mut self, id: SessionId, tag: &LineTag) -> Option<&mut Injection> {
        self.sessions
            .get_mut(&id)?
            .data
            .injections
            .iter_mut()
            .find(|i| i.tag() == *tag)
    }
}

/// Whether `tag` delivers an injection.
pub(crate) fn is_injection(tag: &LineTag) -> bool {
    matches!(tag, LineTag::Internal { key, .. } if key.starts_with(INJECT))
}

/// Waiting injections with their indices.
fn waiting(injections: &[Injection]) -> impl Iterator<Item = (usize, &Injection)> {
    injections
        .iter()
        .enumerate()
        .filter(|(_, i)| i.delivery.is_none())
}

/// Whether a prompt (a queue item or an injection) is on its way to the agent: a run is about
/// to start.
pub(crate) fn prompt_on_its_way(data: &SessionData) -> bool {
    let prompt = |d: &Option<Delivery>| d.is_some_and(|d| d.via() == Via::Prompt);
    data.queue.iter().any(|e| prompt(&e.delivery))
        || data.injections.iter().any(|i| prompt(&i.delivery))
}
