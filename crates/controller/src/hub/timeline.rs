//! Ref-counted session subscriptions: one daemon `session.subscribe` per session, fanned out to
//! every UI subscription on it, so several tabs share one daemon subscription.
//!
//! Each UI subscriber has its own `delivered` seq (the last `head_seq` it was sent). The shared
//! daemon subscription can therefore be (re)opened from the lowest position any subscriber
//! needs: when a subscriber joins behind the shared head, after a reconnect, or when a
//! subscription is replaced. Streamed text (`append_text`) and raw frames are filtered by seq
//! per subscriber, so nobody receives text twice; `upsert` and `end` replace or close an item
//! whole, so repeating them is harmless and they always pass.
//!
//! A new subscriber stays *pending* until its `session.subscribe` response has been sent, so no
//! event reaches the client before the response; its events are buffered meanwhile.
//!
//! Subscriptions that ask for raw frames (the raw view) get a fanout of their own.

use std::collections::BTreeMap;

use controlroom_protocol::Seq;
use controlroom_protocol::daemon::SessionEventsParams;
use controlroom_protocol::ids::{MachineId, SessionId, SubscriptionId};
use controlroom_protocol::session::SessionDetail;
use controlroom_protocol::timeline::{RawFrame, TimelineOp};

use super::clients::ClientId;

/// Identifies a fanout: a session, plus the owning UI subscription for raw-frame fanouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TimelineKey {
    pub machine_id: MachineId,
    pub session_id: SessionId,
    /// `Some` for a dedicated raw-frame fanout.
    pub raw_owner: Option<SubscriptionId>,
}

/// What a new subscriber needs from the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JoinPlan {
    /// The shared daemon subscription is live and exactly at the subscriber's position.
    Join,
    /// (Re)open the daemon subscription.
    Subscribe {
        /// Its `since_seq`.
        since: Seq,
        /// Its `raw_since`, for raw-frame fanouts.
        raw_since: Option<Seq>,
    },
}

/// Events for one subscriber.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Batch {
    pub head_seq: Seq,
    pub ops: Vec<TimelineOp>,
    pub raw: Vec<RawFrame>,
    pub detail: Option<Box<SessionDetail>>,
}

/// A batch addressed to one UI subscription.
#[derive(Debug, PartialEq)]
pub(crate) struct Delivery {
    pub client: ClientId,
    pub subscription_id: SubscriptionId,
    pub batch: Batch,
}

#[derive(Debug)]
struct Subscriber {
    client: ClientId,
    delivered: Seq,
    raw_delivered: Option<Seq>,
    /// `Some` until the subscribe response was sent.
    pending: Option<Vec<Batch>>,
}

/// The fan-out state of one session subscription.
#[derive(Debug, Default)]
pub(crate) struct Fanout {
    /// The current daemon subscription on the live connection.
    daemon_sub: Option<SubscriptionId>,
    /// The latest head the daemon reported.
    head: Seq,
    /// The latest session detail.
    detail: Option<SessionDetail>,
    subscribers: BTreeMap<SubscriptionId, Subscriber>,
}

/// The seq range end of an op that appends; `None` for ops that replace or close whole.
fn append_end(op: &TimelineOp) -> Option<Seq> {
    match op {
        TimelineOp::AppendText { seq_to, .. } => Some(*seq_to),
        _ => None,
    }
}

impl Fanout {
    pub(crate) fn daemon_sub(&self) -> Option<SubscriptionId> {
        self.daemon_sub
    }

    /// The latest head and detail, for a joining subscriber's response.
    pub(crate) fn snapshot(&self) -> (Seq, Option<SessionDetail>) {
        (self.head, self.detail.clone())
    }

    /// Adds a pending subscriber that has seen everything up to `since`.
    pub(crate) fn add(
        &mut self,
        sub: SubscriptionId,
        client: ClientId,
        since: Seq,
        raw_since: Option<Seq>,
    ) -> JoinPlan {
        self.subscribers.insert(
            sub,
            Subscriber {
                client,
                delivered: since,
                raw_delivered: raw_since,
                pending: Some(Vec::new()),
            },
        );
        if self.daemon_sub.is_some() && since == self.head && self.detail.is_some() {
            JoinPlan::Join
        } else {
            self.resume_plan()
                .unwrap_or(JoinPlan::Subscribe { since, raw_since })
        }
    }

    pub(crate) fn has_subscriber(&self, sub: &SubscriptionId, client: ClientId) -> bool {
        self.subscribers
            .get(sub)
            .is_some_and(|s| s.client == client)
    }

    /// Removes a subscriber; returns whether none are left.
    pub(crate) fn remove(&mut self, sub: &SubscriptionId) -> bool {
        self.subscribers.remove(sub);
        self.subscribers.is_empty()
    }

    /// Removes every subscriber of `client`; returns whether none are left.
    pub(crate) fn remove_client(&mut self, client: ClientId) -> bool {
        self.subscribers.retain(|_, s| s.client != client);
        self.subscribers.is_empty()
    }

    /// Where a daemon subscription must start so every subscriber gets what it misses.
    pub(crate) fn resume_plan(&self) -> Option<JoinPlan> {
        let since = self.subscribers.values().map(|s| s.delivered).min()?;
        let raw_since = self
            .subscribers
            .values()
            .filter_map(|s| s.raw_delivered)
            .min();
        Some(JoinPlan::Subscribe { since, raw_since })
    }

    /// Records a new daemon subscription (from its response hook, before any of its events).
    /// Returns the subscription it replaces, to unsubscribe.
    pub(crate) fn subscribed(
        &mut self,
        daemon_sub: SubscriptionId,
        head: Seq,
        detail: SessionDetail,
    ) -> Option<SubscriptionId> {
        self.head = head;
        self.detail = Some(detail);
        self.daemon_sub.replace(daemon_sub)
    }

    /// The daemon connection is gone, and its subscription with it.
    pub(crate) fn disconnected(&mut self) {
        self.daemon_sub = None;
    }

    /// Routes events of daemon subscription `p.subscription_id`; events of any other (replaced)
    /// subscription are ignored.
    pub(crate) fn deliver(&mut self, p: &SessionEventsParams) -> Vec<Delivery> {
        if self.daemon_sub != Some(p.subscription_id) {
            return Vec::new();
        }
        self.head = self.head.max(p.head_seq);
        if let Some(detail) = &p.detail {
            self.detail = Some((**detail).clone());
        }
        let mut out = Vec::new();
        for (id, sub) in &mut self.subscribers {
            let ops: Vec<_> = p
                .ops
                .iter()
                .filter(|op| append_end(op).is_none_or(|end| end > sub.delivered))
                .cloned()
                .collect();
            let raw: Vec<_> = match sub.raw_delivered {
                Some(after) => p.raw.iter().filter(|f| f.seq > after).cloned().collect(),
                None => Vec::new(),
            };
            if ops.is_empty() && raw.is_empty() && p.detail.is_none() && p.head_seq <= sub.delivered
            {
                continue;
            }
            sub.delivered = sub.delivered.max(p.head_seq);
            if let Some(last) = raw.last() {
                sub.raw_delivered = Some(last.seq);
            }
            let batch = Batch {
                head_seq: sub.delivered,
                ops,
                raw,
                detail: p.detail.clone(),
            };
            match &mut sub.pending {
                Some(buffer) => buffer.push(batch),
                None => out.push(Delivery {
                    client: sub.client,
                    subscription_id: *id,
                    batch,
                }),
            }
        }
        out
    }

    /// The subscribe response was sent: returns the buffered batches to send now.
    pub(crate) fn activate(&mut self, sub: &SubscriptionId) -> Vec<Delivery> {
        let Some(s) = self.subscribers.get_mut(sub) else {
            return Vec::new();
        };
        let client = s.client;
        s.pending
            .take()
            .unwrap_or_default()
            .into_iter()
            .map(|batch| Delivery {
                client,
                subscription_id: *sub,
                batch,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::UnixMillis;
    use controlroom_protocol::ids::ItemId;
    use controlroom_protocol::session::{AutoContinueInfo, AutoContinueStatus};
    use controlroom_protocol::timeline::RawStream;

    use super::*;

    fn detail() -> SessionDetail {
        serde_json::from_value(serde_json::json!({
            "agent_sessions": [], "queue": [], "todos": [], "jobs": [], "peers": [],
            "auto_continue": AutoContinueInfo {
                status: AutoContinueStatus::Off, reason: None, runs_used: 0, budget: 0,
            },
        }))
        .unwrap()
    }

    fn text(seq: u64) -> TimelineOp {
        TimelineOp::AppendText {
            id: ItemId::new("m"),
            text: format!("t{seq}"),
            seq_from: Seq(seq),
            seq_to: Seq(seq),
        }
    }

    fn events(sub: SubscriptionId, seqs: std::ops::RangeInclusive<u64>) -> SessionEventsParams {
        SessionEventsParams {
            subscription_id: sub,
            id: SessionId::generate(),
            head_seq: Seq(*seqs.end()),
            ops: seqs.clone().map(text).collect(),
            raw: seqs
                .map(|seq| RawFrame {
                    seq: Seq(seq),
                    ts: UnixMillis(0),
                    stream: RawStream::FromAgent,
                    text: String::new(),
                })
                .collect(),
            detail: None,
        }
    }

    fn texts(d: &Delivery) -> Vec<String> {
        d.batch
            .ops
            .iter()
            .filter_map(|op| match op {
                TimelineOp::AppendText { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn tabs_share_one_daemon_subscription_without_duplicate_text() {
        let mut f = Fanout::default();
        let (c1, c2) = (ClientId(1), ClientId(2));
        let (a, b) = (SubscriptionId::generate(), SubscriptionId::generate());

        let plan = f.add(a, c1, Seq(0), None);
        assert_eq!(
            plan,
            JoinPlan::Subscribe {
                since: Seq(0),
                raw_since: None
            }
        );
        let d1 = SubscriptionId::generate();
        assert_eq!(f.subscribed(d1, Seq(3), detail()), None);
        // Events before the response was sent are buffered.
        assert!(f.deliver(&events(d1, 1..=3)).is_empty());
        let first = f.activate(&a);
        assert_eq!(texts(&first[0]), ["t1", "t2", "t3"]);
        assert!(f.activate(&a).is_empty());

        // A second tab exactly at the shared head joins without the daemon.
        assert_eq!(f.add(b, c2, Seq(3), None), JoinPlan::Join);
        assert!(f.activate(&b).is_empty());
        let both = f.deliver(&events(d1, 4..=4));
        assert_eq!(both.len(), 2);

        // A late joiner from 0 reopens the subscription from 0; the others filter the replayed
        // text, so nobody sees it twice.
        let c = SubscriptionId::generate();
        assert_eq!(
            f.add(c, c2, Seq(0), None),
            JoinPlan::Subscribe {
                since: Seq(0),
                raw_since: None
            }
        );
        let d2 = SubscriptionId::generate();
        assert_eq!(f.subscribed(d2, Seq(5), detail()), Some(d1));
        assert!(
            f.deliver(&events(d1, 5..=5)).is_empty(),
            "replaced subscription"
        );
        let replay = f.deliver(&events(d2, 1..=5));
        assert_eq!(replay.len(), 2);
        assert!(replay.iter().all(|d| texts(d) == ["t5"]));
        assert_eq!(texts(&f.activate(&c)[0]), ["t1", "t2", "t3", "t4", "t5"]);

        assert!(!f.remove(&a));
        assert!(f.remove_client(c2));
    }

    #[test]
    fn reconnects_resume_from_the_lowest_delivered_seq() {
        let mut f = Fanout::default();
        let (a, b) = (SubscriptionId::generate(), SubscriptionId::generate());
        f.add(a, ClientId(1), Seq(0), None);
        let d1 = SubscriptionId::generate();
        f.subscribed(d1, Seq(0), detail());
        f.activate(&a);
        f.deliver(&events(d1, 1..=10));
        assert_eq!(f.add(b, ClientId(2), Seq(10), None), JoinPlan::Join);
        f.activate(&b);
        f.deliver(&events(d1, 11..=12));

        f.disconnected();
        assert!(f.deliver(&events(d1, 13..=13)).is_empty());
        assert_eq!(
            f.resume_plan(),
            Some(JoinPlan::Subscribe {
                since: Seq(12),
                raw_since: None
            })
        );
        let d2 = SubscriptionId::generate();
        f.subscribed(d2, Seq(14), detail());
        let out = f.deliver(&events(d2, 11..=14));
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|d| texts(d) == ["t13", "t14"]));
    }

    #[test]
    fn raw_frames_only_after_the_subscriber_position() {
        let mut f = Fanout::default();
        let a = SubscriptionId::generate();
        assert_eq!(
            f.add(a, ClientId(1), Seq(0), Some(Seq(2))),
            JoinPlan::Subscribe {
                since: Seq(0),
                raw_since: Some(Seq(2))
            }
        );
        let d = SubscriptionId::generate();
        f.subscribed(d, Seq(4), detail());
        f.activate(&a);
        let out = f.deliver(&events(d, 1..=4));
        let seqs: Vec<_> = out[0].batch.raw.iter().map(|r| r.seq.0).collect();
        assert_eq!(seqs, [3, 4]);
    }
}
