//! Pushes waiting to be sent: each waits its delay after the occurrence, and due pushes are
//! held while the owner is active.
//!
//! The queue lives in memory. After a controller restart it is rebuilt from current attention:
//! the first summary of each session raises its pending requests again (`attention::detect`),
//! and delivery dedup drops what was already sent.

use std::collections::HashMap;

use controlroom_protocol::ids::SessionId;
use tokio::time::Instant;

use crate::attention::AttentionEvent;

/// The waiting pushes and the presence hold.
#[derive(Debug, Default)]
pub(crate) struct Queue {
    /// When each occurrence is due.
    due: HashMap<(SessionId, AttentionEvent), Instant>,
    /// While the owner is active: until when, unless they report input again or leave earlier.
    held_until: Option<Instant>,
}

impl Queue {
    /// Adds an occurrence due at `due`; one already waiting keeps its time.
    pub(crate) fn add(&mut self, session: SessionId, event: AttentionEvent, due: Instant) {
        self.due.entry((session, event)).or_insert(due);
    }

    /// Keeps only the session's occurrences that `keep` accepts.
    pub(crate) fn retain_session(
        &mut self,
        session: SessionId,
        keep: impl Fn(&AttentionEvent) -> bool,
    ) {
        self.due
            .retain(|(s, event), _| *s != session || keep(event));
    }

    /// Whether any occurrence is due at `now`.
    pub(crate) fn any_due(&self, now: Instant) -> bool {
        self.due.values().any(|due| *due <= now)
    }

    /// Sets the presence hold, re-evaluated on every look.
    pub(crate) fn set_hold(&mut self, until: Option<Instant>) {
        self.held_until = until;
    }

    /// Takes the occurrences due at `now`, oldest first.
    pub(crate) fn take_due(&mut self, now: Instant) -> Vec<(SessionId, AttentionEvent)> {
        let mut due: Vec<_> = self
            .due
            .iter()
            .filter(|(_, due)| **due <= now)
            .map(|(key, due)| (*due, key.clone()))
            .collect();
        due.sort_by_key(|(due, (session, event))| (*due, *session, event.event_key()));
        for (_, key) in &due {
            self.due.remove(key);
        }
        due.into_iter().map(|(_, key)| key).collect()
    }

    /// When to look again: the earliest due time, but not before the hold ends.
    pub(crate) fn next_wake(&self) -> Option<Instant> {
        let earliest = self.due.values().min().copied()?;
        Some(self.held_until.map_or(earliest, |held| earliest.max(held)))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use controlroom_protocol::ids::RequestId;

    use super::*;
    use crate::attention::{AttentionKind, Occurrence};

    fn approval(id: &str) -> AttentionEvent {
        AttentionEvent {
            kind: AttentionKind::Approval,
            occurrence: Occurrence::Request(RequestId::new(id)),
        }
    }

    #[test]
    fn occurrences_wait_until_due_and_while_held() {
        let t0 = Instant::now();
        let secs = |s| t0 + Duration::from_secs(s);
        let session = SessionId::generate();
        let mut queue = Queue::default();
        assert_eq!(queue.next_wake(), None);
        queue.add(session, approval("r2"), secs(40));
        queue.add(session, approval("r1"), secs(30));
        queue.add(session, approval("r1"), secs(50));
        assert_eq!(queue.next_wake(), Some(secs(30)), "a repeat keeps its time");
        assert!(!queue.any_due(secs(29)));
        assert!(queue.any_due(secs(30)));

        queue.set_hold(Some(secs(100)));
        assert_eq!(queue.next_wake(), Some(secs(100)));
        queue.set_hold(None);
        let taken = queue.take_due(secs(45));
        assert_eq!(
            taken,
            [(session, approval("r1")), (session, approval("r2"))]
        );
        assert_eq!(queue.next_wake(), None);

        queue.add(session, approval("r3"), secs(60));
        queue.add(session, approval("r4"), secs(70));
        queue.retain_session(session, |e| *e != approval("r3"));
        queue.retain_session(SessionId::generate(), |_| false);
        assert_eq!(
            queue.next_wake(),
            Some(secs(70)),
            "only the session's own are dropped"
        );
    }
}
