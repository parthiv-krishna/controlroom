//! Notifications: attention events from the hub's event bus, delivered as Web Push to every
//! device of the owner.
//!
//! - **Dedup:** each device gets at most one push per (session, attention event), claimed in
//!   the store before sending (released again if the send fails, so a later change retries).
//! - **Focus:** a device with a visible client showing the session is skipped.
//! - **Payloads** carry metadata only: session and machine ids and names, the attention kind,
//!   and a deep link. Never code, prompts or tool output.
//! - Subscriptions the push service reports gone (404/410) are deleted.
//! - **SSRF guard:** endpoints must be public `https` host names, are checked again at send
//!   time, must resolve to public addresses, and redirects aren't followed (`endpoint`).
//!
//! In-app notifications need nothing extra: the state feed carries each session's
//! server-owned `attention`, which open tabs show.

mod endpoint;
mod webpush;

use std::sync::{Arc, Weak};
use std::time::Duration;

use controlroom_protocol::UnixMillis;
use controlroom_protocol::agentd;
use controlroom_protocol::ids::{DeviceId, MachineId, SessionId};
use controlroom_protocol::session::SessionSummary;
use controlroom_protocol::ui::NotificationPreferences;
use futures::future::BoxFuture;
use serde::Serialize;
use tokio::sync::broadcast;

pub use webpush::{VapidKey, VapidKeyError, WebPushSender};

use crate::attention::{self, AttentionEvent, AttentionKind};
use crate::hub::{BusEvent, Hub};
use crate::store::{Delivery, PushSubscriptionRow, StoreResult};

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
    /// A legacy daemon's machine; absent for a global session.
    #[serde(skip_serializing_if = "Option::is_none")]
    machine_id: Option<MachineId>,
    session_id: SessionId,
    /// The session's display name.
    name: String,
    /// The machine's name.
    machine: String,
    /// The attention kind.
    state: &'a str,
    /// Deep link relative to the app root.
    url: String,
}

/// The preference keys (one row each, so setting one never overwrites another).
pub(crate) mod prefs {
    use controlroom_protocol::ui::NotificationPreferences;

    use crate::store::{Prefs, StoreResult};

    const ATTENTION: &str = "notifications.attention";
    const QUESTIONS: &str = "notifications.questions";
    const TURN_END: &str = "notifications.turn_end";

    /// The owner's preferences; approvals and problems push by default, questions and finished
    /// runs are in-app only by default.
    pub(crate) fn load(prefs: &Prefs<'_>) -> StoreResult<NotificationPreferences> {
        Ok(NotificationPreferences {
            attention: prefs.get(ATTENTION)?.unwrap_or(true),
            questions: prefs.get(QUESTIONS)?.unwrap_or(false),
            turn_end: prefs.get(TURN_END)?.unwrap_or(false),
        })
    }

    /// Sets the given preferences only.
    pub(crate) fn set(
        prefs: &Prefs<'_>,
        attention: Option<bool>,
        questions: Option<bool>,
        turn_end: Option<bool>,
    ) -> StoreResult<()> {
        for (key, value) in [
            (ATTENTION, attention),
            (QUESTIONS, questions),
            (TURN_END, turn_end),
        ] {
            if let Some(value) = value {
                prefs.set(key, &value)?;
            }
        }
        Ok(())
    }
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
    /// A legacy daemon's machine; `None` for a global session.
    machine_id: Option<MachineId>,
    /// The session's display name.
    name: String,
    /// The machine's name (a global session's primary workspace's machine).
    machine: String,
}

impl Subject {
    /// A legacy daemon's session: the user's name, else the agent's title, else the cwd.
    fn legacy(hub: &Hub, machine_id: MachineId, session: &SessionSummary) -> Self {
        Self {
            session_id: session.id,
            machine_id: Some(machine_id),
            name: session
                .name
                .clone()
                .or_else(|| session.title.clone())
                .unwrap_or_else(|| session.cwd.to_string()),
            machine: hub.machine_name(machine_id).unwrap_or_default(),
        }
    }

    /// A global session: the user's name, else the agent's title, else the primary workspace.
    fn global(hub: &Hub, session: &agentd::SessionSummary) -> Self {
        let primary = session.workspaces.iter().find(|w| w.primary);
        Self {
            session_id: session.id,
            machine_id: None,
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
            machine_id: self.machine_id,
            session_id: self.session_id,
            name: truncate(&self.name),
            machine: truncate(&self.machine),
            state: kind.as_str(),
            url: match self.machine_id {
                Some(machine_id) => format!("#/s/{machine_id}/{}", self.session_id),
                None => format!("#/s/{}", self.session_id),
            },
        }
    }
}

/// Consumes the hub's bus until the hub is gone, pushing attention events.
pub(crate) async fn run(
    hub: Weak<Hub>,
    config: PushConfig,
    mut bus: broadcast::Receiver<BusEvent>,
) {
    loop {
        let event = match bus.recv().await {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(missed = n, "the notifier fell behind the event bus");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let Some(hub) = hub.upgrade() else { return };
        let (subject, events) = match event {
            BusEvent::Session {
                machine_id,
                previous,
                current,
            } => (
                Subject::legacy(&hub, machine_id, &current),
                attention::detect(previous.as_deref(), &*current),
            ),
            BusEvent::GlobalSession { previous, current } => (
                Subject::global(&hub, &current),
                attention::detect(previous.as_deref(), &*current),
            ),
            BusEvent::SessionRemoved { session_id, .. }
            | BusEvent::GlobalSessionRemoved { session_id } => {
                if let Err(e) = hub.repo().push().forget_session(session_id) {
                    tracing::warn!(error = %e, "forgetting a deleted session's pushes");
                }
                continue;
            }
            _ => continue,
        };
        for event in events {
            if let Err(e) = deliver(&hub, &config, &subject, &event).await {
                tracing::warn!(error = %e, "delivering a notification");
            }
        }
    }
}

async fn deliver(
    hub: &Hub,
    config: &PushConfig,
    subject: &Subject,
    event: &AttentionEvent,
) -> StoreResult<()> {
    let repo = hub.repo();
    if !wanted(&prefs::load(&repo.prefs())?, event.kind) {
        return Ok(());
    }
    let body = serde_json::to_vec(&subject.payload(event.kind))?;
    let urgent = event.kind == AttentionKind::Approval;
    let focused: Vec<DeviceId> = hub.focused_devices(subject.session_id);
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
            session_id: subject.session_id,
            event_key: &event.event_key,
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
