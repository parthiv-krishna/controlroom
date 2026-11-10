//! Notifications: attention events from the hub's event bus, delivered as Web Push to every
//! device of the owner.
//!
//! - **Delay:** every push waits the owner's delay (default 30 s, `pending`). When it is due,
//!   it is sent only if its attention is still pending (not answered, read or cleared).
//! - **Presence:** while the owner is active on any device (a visible, focused client with
//!   user input within the idle window, default 2 min) and holding is on (the default), due
//!   pushes go to no device: the active client shows attention in-app. They are re-evaluated
//!   when the user leaves (the idle window passes, or a client hides, blurs or disconnects) and
//!   sent then if still pending, blocking requests with their urgency.
//! - **Dedup:** each device gets at most one push per (session, attention event), claimed in
//!   the store before sending (released again if the send fails, so a later change retries).
//! - **Focus:** a device with a visible client showing the session is skipped.
//! - **Restarts:** waiting pushes are rebuilt from current attention (`pending`).
//! - **Payloads** carry metadata only: the session id, the session's and its primary machine's
//!   names, the attention kind, and a deep link. Never code, prompts or tool output.
//! - Subscriptions the push service reports gone (404/410) are deleted.
//! - **SSRF guard:** endpoints must be public `https` host names, are checked again at send
//!   time, must resolve to public addresses, and redirects aren't followed (`endpoint`).
//!
//! In-app notifications need nothing extra: the state feed carries each session's
//! server-owned `attention`, which open tabs show.

mod endpoint;
mod pending;
pub(crate) mod prefs;
#[cfg(test)]
mod tests;
mod webpush;

use std::sync::{Arc, Weak};
use std::time::Duration;

use controlroom_protocol::UnixMillis;
use controlroom_protocol::ids::{DeviceId, SessionId};
use controlroom_protocol::session::SessionSummary;
use controlroom_protocol::ui::NotificationPreferences;
use futures::future::BoxFuture;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio::time::Instant;

pub use webpush::{VapidKey, VapidKeyError, WebPushSender};

use crate::attention::{self, AttentionEvent, AttentionKind};
use crate::hub::{BusEvent, Hub};
use crate::store::{Delivery, PushSubscriptionRow, StoreResult};
use pending::Queue;

/// How long the dedup log remembers a delivery.
const DEDUP_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Names are cut to keep payloads far below the 4 KB push limit.
const MAX_NAME_CHARS: usize = 120;

/// How one push went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    /// Accepted by the push service.
    Delivered,
    /// The subscription is gone (404/410): delete it.
    Gone,
    /// Failed; the event may be retried.
    Failed(String),
}

/// Sends one encrypted push to a subscription. [`WebPushSender`] is the real one; tests use
/// fakes.
pub trait PushSender: Send + Sync {
    /// Sends `payload`; `urgent` for events that block an agent.
    fn send<'a>(
        &'a self,
        sub: &'a PushSubscriptionRow,
        payload: &'a [u8],
        urgent: bool,
    ) -> BoxFuture<'a, PushOutcome>;
}

/// Web Push configuration.
#[derive(Clone)]
pub struct PushConfig {
    /// The base64url VAPID public key the browser subscribes with.
    pub vapid_public_key: String,
    /// Delivers pushes.
    pub sender: Arc<dyn PushSender>,
}

impl PushConfig {
    /// Web Push with the VAPID private key from a credential file and a contact `subject`
    /// (`mailto:` or `https:`).
    pub fn web_push(
        vapid_key_file: &std::path::Path,
        subject: String,
    ) -> Result<Self, VapidKeyError> {
        let key = VapidKey::load(vapid_key_file)?;
        Ok(Self {
            vapid_public_key: key.public_key_base64url(),
            sender: Arc::new(WebPushSender::new(key, subject)),
        })
    }
}

/// The JSON payload of a push. The service worker builds the deep link from it.
#[derive(Debug, Clone, Serialize)]
struct Payload<'a> {
    session_id: SessionId,
    /// The session's display name.
    name: String,
    /// The primary workspace's machine name.
    machine: String,
    /// The attention kind.
    state: &'a str,
    /// Deep link relative to the app root.
    url: String,
}

fn wanted(prefs: &NotificationPreferences, kind: AttentionKind) -> bool {
    match kind {
        AttentionKind::Approval | AttentionKind::Problem => prefs.attention,
        AttentionKind::Question => prefs.questions,
        AttentionKind::TurnEnd => prefs.turn_end,
    }
}

fn truncate(s: &str) -> String {
    s.chars().take(MAX_NAME_CHARS).collect()
}

/// The session a notification is about, as its payload shows it.
struct Subject {
    session_id: SessionId,
    /// The session's display name.
    name: String,
    /// The primary workspace's machine name.
    machine: String,
}

impl Subject {
    /// The user's name, else the agent's title, else the primary workspace's path.
    fn of(hub: &Hub, session: &SessionSummary) -> Self {
        let primary = session.workspaces.iter().find(|w| w.primary);
        Self {
            session_id: session.id,
            name: session
                .name
                .clone()
                .or_else(|| session.title.clone())
                .or_else(|| primary.map(|w| w.path.to_string()))
                .unwrap_or_default(),
            machine: primary
                .and_then(|w| hub.machine_name(w.machine_id))
                .unwrap_or_default(),
        }
    }

    fn payload<'a>(&self, kind: AttentionKind) -> Payload<'a> {
        Payload {
            session_id: self.session_id,
            name: truncate(&self.name),
            machine: truncate(&self.machine),
            state: kind.as_str(),
            url: format!("#/s/{}", self.session_id),
        }
    }
}

/// Consumes the hub's bus until the hub is gone: queues attention events and sends them when
/// due, unless the owner is active.
pub(crate) async fn run(
    hub: Weak<Hub>,
    config: PushConfig,
    mut bus: broadcast::Receiver<BusEvent>,
) {
    let mut queue = Queue::default();
    loop {
        let wake = queue.next_wake();
        let event = tokio::select! {
            event = bus.recv() => Some(event),
            () = sleep_until(wake) => None,
        };
        let Some(hub) = hub.upgrade() else { return };
        match event {
            Some(Ok(event)) => observe(&hub, &mut queue, event),
            Some(Err(broadcast::error::RecvError::Lagged(n))) => {
                tracing::warn!(missed = n, "the notifier fell behind the event bus");
            }
            Some(Err(broadcast::error::RecvError::Closed)) => return,
            None => {}
        }
        flush(&hub, &config, &mut queue).await;
    }
}

/// Sleeps until `at`, or forever.
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Queues a session's new occurrences (if they push at all) and drops the ones dealt with.
fn observe(hub: &Hub, queue: &mut Queue, event: BusEvent) {
    match event {
        BusEvent::Session { previous, current } => {
            queue.retain_session(current.id, |e| e.still_pending(&current));
            let events = attention::detect(previous.as_deref(), &current);
            if events.is_empty() {
                return;
            }
            let prefs = match prefs::load(&hub.repo().prefs()) {
                Ok(prefs) => prefs,
                Err(e) => {
                    tracing::warn!(error = %e, "reading the notification preferences");
                    return;
                }
            };
            let due = Instant::now() + Duration::from_secs(prefs.delay_s.into());
            for event in events.into_iter().filter(|e| wanted(&prefs, e.kind)) {
                queue.add(current.id, event, due);
            }
        }
        BusEvent::SessionRemoved { session_id } => {
            queue.retain_session(session_id, |_| false);
            if let Err(e) = hub.repo().push().forget_session(session_id) {
                tracing::warn!(error = %e, "forgetting a deleted session's pushes");
            }
        }
        _ => {}
    }
}

/// Sends the due pushes unless the owner is active (then they wait until the user leaves).
async fn flush(hub: &Hub, config: &PushConfig, queue: &mut Queue) {
    let now = Instant::now();
    if !queue.any_due(now) {
        return;
    }
    let prefs = match prefs::load(&hub.repo().prefs()) {
        Ok(prefs) => prefs,
        Err(e) => {
            tracing::warn!(error = %e, "reading the notification preferences; dropping due pushes");
            queue.take_due(now);
            return;
        }
    };
    let held = if prefs.hold_while_active {
        hub.active_until(Duration::from_secs(prefs.idle_s.into()))
    } else {
        None
    };
    queue.set_hold(held);
    if held.is_some() {
        return;
    }
    for (session, event) in queue.take_due(now) {
        if let Err(e) = deliver(hub, config, &prefs, session, &event).await {
            tracing::warn!(error = %e, "delivering a notification");
        }
    }
}

/// Pushes one occurrence to every device, if it is still wanted and pending.
async fn deliver(
    hub: &Hub,
    config: &PushConfig,
    prefs: &NotificationPreferences,
    session_id: SessionId,
    event: &AttentionEvent,
) -> StoreResult<()> {
    if !wanted(prefs, event.kind) {
        return Ok(());
    }
    let Some(session) = hub.session(session_id).filter(|s| event.still_pending(s)) else {
        return Ok(());
    };
    let subject = Subject::of(hub, &session);
    let body = serde_json::to_vec(&subject.payload(event.kind))?;
    let urgent = event.kind == AttentionKind::Approval;
    let event_key = event.event_key();
    let focused: Vec<DeviceId> = hub.focused_devices(session_id);
    let repo = hub.repo();
    let push = repo.push();
    push.prune_deliveries(UnixMillis(
        UnixMillis::now().0 - DEDUP_RETENTION.as_millis() as i64,
    ))?;
    for sub in push.list()? {
        if focused.contains(&sub.device_id) {
            continue;
        }
        let delivery = Delivery {
            device_id: &sub.device_id,
            session_id,
            event_key: &event_key,
        };
        if !push.claim(delivery)? {
            continue;
        }
        match config.sender.send(&sub, &body, urgent).await {
            PushOutcome::Delivered => push.mark_success(&sub.device_id, UnixMillis::now())?,
            PushOutcome::Gone => {
                tracing::info!(device = %sub.device_id, "pruning a gone push subscription");
                push.delete(&sub.device_id)?;
                hub.device_changed(&sub.device_id);
            }
            PushOutcome::Failed(why) => {
                tracing::warn!(device = %sub.device_id, %why, "push failed");
                push.release(delivery)?;
            }
        }
    }
    Ok(())
}

/// Parses a browser's `PushSubscription.toJSON()` for `device`. Only public https endpoints
/// are accepted (`endpoint::check`).
pub(crate) fn parse_subscription(
    device: &DeviceId,
    subscription: &serde_json::Value,
) -> Result<PushSubscriptionRow, controlroom_protocol::RpcError> {
    let field = |v: &serde_json::Value| v.as_str().map(str::to_owned);
    let (Some(endpoint), Some(p256dh), Some(auth)) = (
        field(&subscription["endpoint"]),
        field(&subscription["keys"]["p256dh"]),
        field(&subscription["keys"]["auth"]),
    ) else {
        return Err(crate::errors::invalid_params(
            "subscription must be a PushSubscription with endpoint and keys",
        ));
    };
    if let Err(why) = endpoint::check(&endpoint) {
        return Err(crate::errors::invalid_params(format!(
            "invalid push endpoint: {why}"
        )));
    }
    Ok(PushSubscriptionRow {
        device_id: device.clone(),
        endpoint,
        p256dh,
        auth,
    })
}
