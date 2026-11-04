//! The aggregated state feed's cursor logic (`state.subscribe` / `state.events`): one epoch per
//! hub lifetime, a revision per change, and a bounded log of recent changes so a client that
//! reconnects catches up without a snapshot.

use std::collections::VecDeque;

use controlroom_protocol::Rev;
use controlroom_protocol::ids::Epoch;
use controlroom_protocol::ui::{StateChange, StateCursor, StateEvent};

/// How many recent changes a feed keeps for catching up.
pub(crate) const RETAINED_CHANGES: usize = 4096;

/// One owner's change log.
#[derive(Debug)]
pub(crate) struct StateFeed {
    epoch: Epoch,
    rev: u64,
    log: VecDeque<StateChange>,
    capacity: usize,
}

impl StateFeed {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            epoch: Epoch::generate(),
            rev: 0,
            log: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    pub(crate) fn cursor(&self) -> StateCursor {
        StateCursor {
            epoch: self.epoch,
            rev: Rev(self.rev),
        }
    }

    /// Records a change and returns it with its revision.
    pub(crate) fn push(&mut self, event: StateEvent) -> StateChange {
        self.rev += 1;
        let change = StateChange {
            rev: Rev(self.rev),
            event,
        };
        if self.log.len() == self.capacity {
            self.log.pop_front();
        }
        self.log.push_back(change.clone());
        change
    }

    /// Every change after `since`, or `None` when the client needs a snapshot: no cursor,
    /// another epoch (the controller restarted), a cursor from the future, or one older than
    /// the retained log.
    pub(crate) fn changes_since(&self, since: Option<StateCursor>) -> Option<Vec<StateChange>> {
        let since = since?;
        if since.epoch != self.epoch || since.rev.0 > self.rev {
            return None;
        }
        let oldest_needed = since.rev.0 + 1;
        let oldest_kept = self.log.front().map_or(self.rev + 1, |c| c.rev.0);
        if oldest_needed < oldest_kept {
            return None;
        }
        Some(
            self.log
                .iter()
                .filter(|c| c.rev.0 > since.rev.0)
                .cloned()
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use controlroom_protocol::ids::ProjectId;

    fn removed() -> StateEvent {
        StateEvent::ProjectRemoved {
            project_id: ProjectId::generate(),
        }
    }

    #[test]
    fn catch_up_within_the_log_and_snapshot_otherwise() {
        let mut feed = StateFeed::new(3);
        let at = |rev| StateCursor {
            epoch: feed.epoch,
            rev: Rev(rev),
        };
        assert_eq!(feed.changes_since(None), None);
        assert_eq!(feed.changes_since(Some(at(0))), Some(vec![]));
        let (c5, c3, c2, c1, c6) = (at(5), at(3), at(2), at(1), at(6));
        for _ in 0..5 {
            feed.push(removed());
        }
        assert_eq!(feed.cursor(), c5);
        let revs = |c: Option<Vec<StateChange>>| {
            c.map(|v| v.into_iter().map(|c| c.rev.0).collect::<Vec<_>>())
        };
        assert_eq!(revs(feed.changes_since(Some(c5))), Some(vec![]));
        assert_eq!(revs(feed.changes_since(Some(c3))), Some(vec![4, 5]));
        assert_eq!(revs(feed.changes_since(Some(c2))), Some(vec![3, 4, 5]));
        assert_eq!(feed.changes_since(Some(c1)), None, "too old");
        assert_eq!(feed.changes_since(Some(c6)), None, "from the future");
        let other = StateCursor {
            epoch: Epoch::generate(),
            rev: Rev(5),
        };
        assert_eq!(feed.changes_since(Some(other)), None, "another epoch");
    }
}
