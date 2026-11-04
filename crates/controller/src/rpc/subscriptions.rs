//! UI session subscriptions over the shared daemon subscriptions of the hub's fanouts:
//! subscribing, resubscribing after a reconnect, and ending them. The same logic serves legacy
//! daemons' sessions (old UI API) and the agent daemon's global sessions (next UI API); only
//! the [`Source`] differs.

use std::sync::{Arc, Mutex};

use controlroom_client_core::daemon::{CallOptions, DaemonClient};
use controlroom_protocol::daemon::methods::SessionSubscribe;
use controlroom_protocol::daemon::{
    SessionSubscribeParams, SessionSubscribeResult, UnsubscribeParams,
};
use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::{MachineId, SubscriptionId};
use controlroom_protocol::{Ack, RpcError, RpcOutcome, Seq};

use super::{Conn, Reply, reply};
use crate::errors::{call_error, not_found};
use crate::hub::{Hub, JoinPlan, Source, TimelineKey, client_gone};

/// Serializes subscription changes per source.
fn sub_lock(hub: &Hub, source: Source) -> Result<Arc<tokio::sync::Mutex<()>>, RpcError> {
    match source {
        Source::Machine(machine_id) => hub
            .lock()
            .machines
            .get(&machine_id)
            .map(|e| e.sub_lock.clone())
            .ok_or_else(|| not_found(Resource::Machine, machine_id)),
        Source::Agentd => Ok(hub.agentd_sub_lock.clone()),
    }
}

/// The source's connected daemon.
fn daemon(hub: &Hub, source: Source) -> Result<DaemonClient, RpcError> {
    match source {
        Source::Machine(machine_id) => hub.daemon(machine_id),
        Source::Agentd => hub.agentd(),
    }
}

/// A failed call to the source, for the UI.
fn source_error(
    hub: &Hub,
    source: Source,
    e: controlroom_client_core::daemon::CallError,
) -> RpcError {
    match source {
        Source::Machine(machine_id) => call_error(e, machine_id, || hub.connectivity(machine_id)),
        Source::Agentd => crate::rpc::passthrough::agentd_error(e),
    }
}

/// Opens (or replaces) the daemon subscription of a fanout. The response hook records it
/// before any of its events are routed. Returns the head and detail.
async fn open(
    hub: &Arc<Hub>,
    daemon: &DaemonClient,
    key: TimelineKey,
    since: Seq,
    raw_since: Option<Seq>,
) -> Result<SessionSubscribeResult, RpcError> {
    // The subscription the hook displaced (or orphaned), to end once the call returns.
    let ended: Arc<Mutex<Option<SubscriptionId>>> = Arc::default();
    let (hook_hub, hook_ended) = (hub.clone(), ended.clone());
    let options = CallOptions {
        on_response: Some(Box::new(move |response| {
            let RpcOutcome::Result(value) = &response.outcome else {
                return;
            };
            let Ok(r) = serde_json::from_value::<SessionSubscribeResult>(value.clone()) else {
                return;
            };
            let mut st = hook_hub.lock();
            let displaced = match st.timelines.get_mut(&key) {
                Some(fanout) => fanout.subscribed(r.subscription_id, r.head_seq, r.detail),
                // Every subscriber left meanwhile.
                None => Some(r.subscription_id),
            };
            *hook_ended.lock().unwrap_or_else(|p| p.into_inner()) = displaced;
        })),
        ..CallOptions::default()
    };
    let params = SessionSubscribeParams {
        id: key.session_id,
        since_seq: since,
        raw_since,
        timeline: key.timeline,
    };
    let result = daemon
        .call_with::<SessionSubscribe>(&params, options)
        .await
        .map_err(|e| source_error(hub, key.source, e))?;
    if let Some(old) = ended.lock().unwrap_or_else(|p| p.into_inner()).take() {
        hub.unsubscribe_source(key.source, old);
    }
    Ok(result)
}

/// `session.subscribe`: joins the session's shared subscription, or (re)opens it from the
/// lowest position any subscriber needs. The new subscription stays pending (its events are
/// buffered) until the response is queued.
pub(super) async fn subscribe(
    conn: &Arc<Conn>,
    source: Source,
    p: SessionSubscribeParams,
) -> Result<Reply, RpcError> {
    let hub = &conn.hub;
    let lock = sub_lock(hub, source)?;
    let _guard = lock.lock().await;
    let daemon = daemon(hub, source)?;
    let ui_sub = SubscriptionId::generate();
    let key = TimelineKey {
        source,
        session_id: p.id,
        raw_owner: p.raw_since.map(|_| ui_sub),
        timeline: p.timeline,
    };
    let (plan, joined) = {
        let mut st = hub.lock();
        // A client that closed meanwhile must not leave a subscription behind.
        if !st.clients.contains_key(&conn.client) {
            return Err(client_gone());
        }
        let fanout = st.timelines.entry(key).or_default();
        let plan = fanout.add(ui_sub, conn.client, p.since_seq, p.raw_since, key.timeline);
        (plan, fanout.snapshot())
    };
    let (head_seq, detail) = match plan {
        JoinPlan::Join => (joined.0, joined.1),
        JoinPlan::Subscribe { since, raw_since } => {
            match open(hub, &daemon, key, since, raw_since).await {
                Ok(r) => (r.head_seq, Some(r.detail)),
                Err(e) => {
                    let mut st = hub.lock();
                    if let Some(fanout) = st.timelines.get_mut(&key)
                        && fanout.remove(&ui_sub)
                        && fanout.daemon_sub().is_none()
                    {
                        st.timelines.remove(&key);
                    }
                    return Err(e);
                }
            }
        }
    };
    let detail = detail.ok_or_else(|| crate::errors::internal("no session detail yet"))?;
    let mut reply = reply(SessionSubscribeResult {
        subscription_id: ui_sub,
        head_seq,
        detail,
    })?;
    let hub = hub.clone();
    reply.after = Some(Box::new(move || {
        let mut st = hub.lock();
        let deliveries = st
            .timelines
            .get_mut(&key)
            .map(|f| f.activate(&ui_sub))
            .unwrap_or_default();
        st.send_timeline(key, deliveries);
    }));
    Ok(reply)
}

/// `unsubscribe`: ends a state or session subscription of this connection; a session's daemon
/// subscription ends with its last subscriber.
pub(super) async fn unsubscribe(conn: &Conn, p: UnsubscribeParams) -> Result<Ack, RpcError> {
    let hub = &conn.hub;
    let sub = p.subscription_id;
    let found = {
        let mut st = hub.lock();
        let state = st
            .clients
            .get_mut(&conn.client)
            .and_then(|c| c.state_subs.remove(&sub));
        if state.is_some() {
            return Ok(Ack {});
        }
        st.timelines
            .iter()
            .find(|(_, f)| f.has_subscriber(&sub, conn.client))
            .map(|(key, _)| *key)
    };
    let Some(key) = found else {
        return Err(not_found(Resource::Subscription, sub));
    };
    let lock = sub_lock(hub, key.source)?;
    let _guard = lock.lock().await;
    let ended = {
        let mut st = hub.lock();
        let emptied = st.timelines.get_mut(&key).is_some_and(|f| f.remove(&sub));
        if emptied {
            st.timelines.remove(&key).and_then(|f| f.daemon_sub())
        } else {
            None
        }
    };
    if let Some(daemon_sub) = ended {
        hub.unsubscribe_source(key.source, daemon_sub);
    }
    Ok(Ack {})
}

/// After a reconnect: resumes every session subscription of the source from the lowest seq its
/// subscribers have, so they get exactly what they missed.
async fn resubscribe(hub: &Arc<Hub>, source: Source, daemon: &DaemonClient) {
    let Ok(lock) = sub_lock(hub, source) else {
        return;
    };
    let _guard = lock.lock().await;
    let wanted: Vec<_> = {
        let st = hub.lock();
        st.timelines
            .iter()
            .filter(|(k, f)| k.source == source && f.daemon_sub().is_none())
            .filter_map(|(k, f)| Some((*k, f.resume_plan()?)))
            .collect()
    };
    for (key, plan) in wanted {
        let JoinPlan::Subscribe { since, raw_since } = plan else {
            continue;
        };
        if let Err(e) = open(hub, daemon, key, since, raw_since).await {
            tracing::warn!(?source, session = %key.session_id, error = %e.message, "resubscribing a session");
        }
    }
}

/// After a legacy daemon reconnected.
pub(crate) async fn resubscribe_sessions(
    hub: &Arc<Hub>,
    machine_id: MachineId,
    daemon: &DaemonClient,
) {
    resubscribe(hub, Source::Machine(machine_id), daemon).await;
}

/// After the agent daemon reconnected.
pub(crate) async fn resubscribe_global(hub: &Arc<Hub>, daemon: &DaemonClient) {
    resubscribe(hub, Source::Agentd, daemon).await;
}
