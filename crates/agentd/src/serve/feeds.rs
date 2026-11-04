//! `sessions.subscribe`, `session.subscribe` and `hostops.subscribe` as notification feeds.
//!
//! The subscribe result is queued before the feed task starts, so it precedes every
//! notification of the subscription. A feed that lags behind the runtime's broadcast
//! resubscribes from the last rev or `head_seq` it sent, so the client sees no gap. If the
//! sessions rev log no longer reaches back that far, or the host bridge feed lags (its pending
//! requests can only be resent whole), the feed can't be repaired in place and the connection
//! is closed; the controller reconnects and resubscribes (disconnecting does nothing else). A detail-only session subscription (`timeline: false`) gets no backlog and
//! no timeline ops, only the detail when it changes (and raw frames if it asked for them).

use std::sync::Arc;

use controlroom_protocol::agentd::notifications::{
    HostOpsRequest, HostOpsWithdraw, SessionEvents, SessionsEvents,
};
use controlroom_protocol::agentd::{
    HostOpsRequestParams, HostOpsSubscribeResult, HostOpsWithdrawParams, SessionEventsParams,
    SessionSubscribeParams, SessionSubscribeResult, SessionsChange, SessionsEventsParams,
    SessionsSubscribeParams, SessionsSubscribeResult,
};
use controlroom_protocol::ids::{Rev, Seq, SessionId, SubscriptionId};
use controlroom_protocol::timeline::{RawFrame, TimelineItem, TimelineOp};
use controlroom_protocol::{RpcError, RpcId, RpcNotification};
use controlroom_runtime::{BridgeEvent, SessionUpdate};
use tokio::sync::broadcast::{Receiver, error::RecvError, error::TryRecvError};

use super::conn::Conn;

/// Updates merged into one notification at most.
const BATCH: usize = 64;

fn to_value<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, RpcError> {
    serde_json::to_value(value).map_err(super::dispatch::internal)
}

/// `sessions.subscribe`.
pub async fn subscribe_sessions(conn: &Arc<Conn>, id: RpcId, params: SessionsSubscribeParams) {
    let subscription = match conn.shared.runtime.subscribe_sessions(params.since_rev) {
        Ok(s) => s,
        Err(e) => return conn.respond(id, Err(e)).await,
    };
    let subscription_id = SubscriptionId::generate();
    let result = SessionsSubscribeResult {
        subscription_id,
        rev: subscription.rev,
        snapshot: subscription.snapshot,
    };
    let feed = sessions_feed(
        conn.clone(),
        subscription_id,
        subscription.rev,
        subscription.backlog,
        subscription.changes,
    );
    start_after_result(conn, id, subscription_id, to_value(&result), feed).await;
}

/// Registers the subscription before answering, so an `unsubscribe` sent right after the
/// result always finds it (requests run concurrently), and starts the feed only once the
/// result is queued, so the result precedes every notification.
async fn start_after_result(
    conn: &Arc<Conn>,
    id: RpcId,
    subscription_id: SubscriptionId,
    result: Result<serde_json::Value, RpcError>,
    feed: impl std::future::Future<Output = ()> + Send + 'static,
) {
    let (go, ready) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        if ready.await.is_ok() {
            feed.await;
        }
    });
    conn.add_subscription(subscription_id, task.abort_handle());
    conn.respond(id, result).await;
    // The client may already have unsubscribed (the task is then aborted): nothing to start.
    let _ = go.send(());
}

async fn sessions_feed(
    conn: Arc<Conn>,
    subscription_id: SubscriptionId,
    rev: Rev,
    backlog: Vec<SessionsChange>,
    mut changes: Receiver<SessionsChange>,
) {
    let mut last = rev;
    let mut batch = backlog;
    loop {
        batch.retain(|c| c.rev > last);
        if let Some(change) = batch.last() {
            last = change.rev;
            let params = SessionsEventsParams {
                subscription_id,
                changes: std::mem::take(&mut batch),
            };
            conn.send(&RpcNotification::new::<SessionsEvents>(params))
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
            Err(RecvError::Lagged(_)) => match conn.shared.runtime.subscribe_sessions(Some(last)) {
                Ok(again) if again.snapshot.is_none() => {
                    batch = again.backlog;
                    changes = again.changes;
                }
                _ => {
                    tracing::warn!("sessions feed lagged past the rev log; closing the connection");
                    conn.close();
                    return;
                }
            },
            Err(RecvError::Closed) => return,
        }
    }
}

/// `hostops.subscribe`: every pending host request, then new ones and withdrawals.
pub async fn subscribe_hostops(conn: &Arc<Conn>, id: RpcId) {
    let subscription = conn.shared.runtime.hostops_subscribe();
    let subscription_id = subscription.subscription_id;
    let result = HostOpsSubscribeResult {
        subscription_id,
        requests: subscription.requests,
    };
    let feed = hostops_feed(conn.clone(), subscription_id, subscription.events);
    start_after_result(conn, id, subscription_id, to_value(&result), feed).await;
}

async fn hostops_feed(
    conn: Arc<Conn>,
    subscription_id: SubscriptionId,
    mut events: Receiver<BridgeEvent>,
) {
    loop {
        let first = match events.recv().await {
            Ok(event) => event,
            Err(RecvError::Lagged(_)) => {
                tracing::warn!("the host bridge feed lagged; closing the connection");
                conn.close();
                return;
            }
            Err(RecvError::Closed) => return,
        };
        let mut batch = vec![first];
        while batch.len() < BATCH {
            match events.try_recv() {
                Ok(event) => batch.push(event),
                Err(_) => break,
            }
        }
        let mut requests = Vec::new();
        for event in batch {
            match event {
                BridgeEvent::Request(request) => requests.push(*request),
                BridgeEvent::Withdraw(id) => {
                    send_requests(&conn, subscription_id, std::mem::take(&mut requests)).await;
                    let params = HostOpsWithdrawParams {
                        subscription_id,
                        ids: vec![id],
                    };
                    conn.send(&RpcNotification::new::<HostOpsWithdraw>(params))
                        .await;
                }
            }
        }
        send_requests(&conn, subscription_id, requests).await;
    }
}

async fn send_requests(
    conn: &Conn,
    subscription_id: SubscriptionId,
    requests: Vec<controlroom_protocol::agentd::HostOpRequest>,
) {
    if requests.is_empty() {
        return;
    }
    let params = HostOpsRequestParams {
        subscription_id,
        requests,
    };
    conn.send(&RpcNotification::new::<HostOpsRequest>(params))
        .await;
}

/// `session.subscribe`.
pub async fn subscribe_session(conn: &Arc<Conn>, id: RpcId, params: SessionSubscribeParams) {
    let runtime = &conn.shared.runtime;
    let since = params.timeline.then_some(params.since_seq);
    let subscription = match runtime
        .subscribe_session(params.id, since, params.raw_since)
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
    let feed = SessionFeed {
        conn: conn.clone(),
        subscription_id,
        session: params.id,
        raw: params.raw_since.is_some(),
        timeline: params.timeline,
        head_seq: subscription.head_seq,
    };
    let run = feed.run(subscription.backlog, subscription.raw, subscription.updates);
    start_after_result(conn, id, subscription_id, to_value(&result), run).await;
}

struct SessionFeed {
    conn: Arc<Conn>,
    subscription_id: SubscriptionId,
    session: SessionId,
    /// The client asked for raw frames.
    raw: bool,
    /// The client asked for timeline ops (`false`: detail only).
    timeline: bool,
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
                    match runtime
                        .subscribe_session(self.session, self.timeline.then_some(since), raw_since)
                        .await
                    {
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
        if self.timeline {
            events.ops.extend(update.ops);
        }
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
