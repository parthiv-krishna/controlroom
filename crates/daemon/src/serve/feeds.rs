//! `machine.subscribe` and `session.subscribe` as notification feeds.
//!
//! The subscribe result is queued before the feed task starts, so it precedes every
//! notification of the subscription. A feed that lags behind the runtime's broadcast
//! resubscribes from the last rev or `head_seq` it sent, so the client sees no gap. If the
//! machine rev log no longer reaches back that far, the feed can't be repaired in place and
//! the connection is closed; the client reconnects and resubscribes (disconnecting does
//! nothing else).

use std::sync::Arc;

use controlroom_protocol::daemon::notifications::{MachineEvents, SessionEvents};
use controlroom_protocol::daemon::{
    MachineEventsParams, MachineSubscribeParams, MachineSubscribeResult, SessionEventsParams,
    SessionSubscribeParams, SessionSubscribeResult,
};
use controlroom_protocol::ids::{Rev, Seq, SessionId, SubscriptionId};
use controlroom_protocol::timeline::{RawFrame, TimelineItem, TimelineOp};
use controlroom_protocol::{RpcError, RpcId, RpcNotification};
use controlroom_runtime::SessionUpdate;
use tokio::sync::broadcast::{Receiver, error::RecvError, error::TryRecvError};

use super::conn::Conn;

/// Updates merged into one notification at most.
const BATCH: usize = 64;

fn to_value<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, RpcError> {
    serde_json::to_value(value).map_err(super::dispatch::internal)
}

/// `machine.subscribe`.
pub async fn subscribe_machine(conn: &Arc<Conn>, id: RpcId, params: MachineSubscribeParams) {
    let subscription = match conn.shared.runtime.subscribe_machine(params.since_rev) {
        Ok(s) => s,
        Err(e) => return conn.respond(id, Err(e)).await,
    };
    let subscription_id = SubscriptionId::generate();
    let result = MachineSubscribeResult {
        subscription_id,
        rev: subscription.rev,
        snapshot: subscription.snapshot,
    };
    conn.respond(id, to_value(&result)).await;
    let feed = machine_feed(
        conn.clone(),
        subscription_id,
        subscription.rev,
        subscription.backlog,
        subscription.changes,
    );
    conn.add_subscription(subscription_id, tokio::spawn(feed).abort_handle());
}

async fn machine_feed(
    conn: Arc<Conn>,
    subscription_id: SubscriptionId,
    rev: Rev,
    backlog: Vec<controlroom_protocol::daemon::MachineChange>,
    mut changes: Receiver<controlroom_protocol::daemon::MachineChange>,
) {
    let mut last = rev;
    let mut batch = backlog;
    loop {
        batch.retain(|c| c.rev > last);
        if let Some(change) = batch.last() {
            last = change.rev;
            let params = MachineEventsParams {
                subscription_id,
                changes: std::mem::take(&mut batch),
            };
            conn.send(&RpcNotification::new::<MachineEvents>(params))
                .await;
        }
        match changes.recv().await {
            Ok(change) => {
                batch.push(change);
                while batch.len() < BATCH {
                    match changes.try_recv() {
                        Ok(change) => batch.push(change),
                        Err(TryRecvError::Lagged(_)) => break,
                        Err(_) => break,
                    }
                }
            }
            Err(RecvError::Lagged(_)) => match conn.shared.runtime.subscribe_machine(Some(last)) {
                Ok(again) if again.snapshot.is_none() => {
                    batch = again.backlog;
                    changes = again.changes;
                }
                _ => {
                    tracing::warn!("machine feed lagged past the rev log; closing the connection");
                    conn.close();
                    return;
                }
            },
            Err(RecvError::Closed) => return,
        }
    }
}

/// `session.subscribe`.
pub async fn subscribe_session(conn: &Arc<Conn>, id: RpcId, params: SessionSubscribeParams) {
    let runtime = &conn.shared.runtime;
    let subscription = match runtime
        .subscribe_session(params.id, params.since_seq, params.raw_since)
        .await
    {
        Ok(s) => s,
        Err(e) => return conn.respond(id, Err(e)).await,
    };
    let subscription_id = SubscriptionId::generate();
    let result = SessionSubscribeResult {
        subscription_id,
        head_seq: subscription.head_seq,
        detail: subscription.detail,
    };
    conn.respond(id, to_value(&result)).await;
    let feed = SessionFeed {
        conn: conn.clone(),
        subscription_id,
        session: params.id,
        raw: params.raw_since.is_some(),
        head_seq: subscription.head_seq,
    };
    let task = tokio::spawn(feed.run(
        subscription.backlog,
        subscription.raw,
        subscription.updates,
    ));
    conn.add_subscription(subscription_id, task.abort_handle());
}

struct SessionFeed {
    conn: Arc<Conn>,
    subscription_id: SubscriptionId,
    session: SessionId,
    /// The client asked for raw frames.
    raw: bool,
    /// The last `head_seq` sent.
    head_seq: Seq,
}

impl SessionFeed {
    async fn run(
        mut self,
        backlog: Vec<TimelineItem>,
        raw: Vec<RawFrame>,
        mut updates: Receiver<SessionUpdate>,
    ) {
        self.send_backlog(backlog, raw, None).await;
        loop {
            let first = match updates.recv().await {
                Ok(update) => update,
                Err(RecvError::Lagged(_)) => {
                    let runtime = &self.conn.shared.runtime;
                    let since = self.head_seq;
                    let raw_since = self.raw.then_some(since);
                    match runtime.subscribe_session(self.session, since, raw_since).await {
                        Ok(again) => {
                            self.head_seq = again.head_seq;
                            updates = again.updates;
                            self.send_backlog(again.backlog, again.raw, Some(again.detail))
                                .await;
                        }
                        Err(e) => {
                            tracing::warn!("session feed {}: resubscribing: {e}", self.session);
                            return;
                        }
                    }
                    continue;
                }
                Err(RecvError::Closed) => return,
            };
            let mut events = self.events(first.head_seq);
            self.add(&mut events, first);
            for _ in 1..BATCH {
                match updates.try_recv() {
                    Ok(update) => self.add(&mut events, update),
                    Err(_) => break,
                }
            }
            if !events.ops.is_empty() || !events.raw.is_empty() || events.detail.is_some() {
                self.head_seq = events.head_seq;
                self.conn
                    .send(&RpcNotification::new::<SessionEvents>(events))
                    .await;
            }
        }
    }

    fn events(&self, head_seq: Seq) -> SessionEventsParams {
        SessionEventsParams {
            subscription_id: self.subscription_id,
            id: self.session,
            head_seq,
            ops: Vec::new(),
            raw: Vec::new(),
            detail: None,
        }
    }

    fn add(&self, events: &mut SessionEventsParams, update: SessionUpdate) {
        events.head_seq = events.head_seq.max(update.head_seq);
        events.ops.extend(update.ops);
        if self.raw {
            events.raw.extend(update.raw);
        }
        if let Some(detail) = update.detail {
            events.detail = Some(Box::new(detail));
        }
    }

    /// Sends a (re)subscription's backlog as upserts.
    async fn send_backlog(
        &mut self,
        backlog: Vec<TimelineItem>,
        raw: Vec<RawFrame>,
        detail: Option<controlroom_protocol::session::SessionDetail>,
    ) {
        if backlog.is_empty() && raw.is_empty() && detail.is_none() {
            return;
        }
        let mut events = self.events(self.head_seq);
        events.ops = backlog
            .into_iter()
            .map(|item| TimelineOp::Upsert {
                item: Box::new(item),
            })
            .collect();
        if self.raw {
            events.raw = raw;
        }
        events.detail = detail.map(Box::new);
        self.conn
            .send(&RpcNotification::new::<SessionEvents>(events))
            .await;
    }
}
