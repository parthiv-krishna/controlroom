//! The notifier against a real hub on paused time: delay, presence hold and release, focus,
//! dedup and restarts. Sessions come in as the agent daemon's feed would deliver them; clients
//! are registered like UI connections.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use controlroom_client_core::MasterKey;
use controlroom_protocol::Rev;
use controlroom_protocol::agentd::{
    SessionsChange, SessionsEvent, SessionsEventsParams, SessionsSubscribeResult,
};
use controlroom_protocol::ids::{DeviceId, SessionId, SubscriptionId};
use controlroom_protocol::session::{RequestState, SessionSummary};
use controlroom_protocol::timeline::NoticeLevel;
use controlroom_protocol::ui::{AuthMode, NotificationsSetPreferencesParams};
use futures::future::BoxFuture;
use tokio::sync::{mpsc, watch};

use super::{PushConfig, PushOutcome, PushSender, prefs};
use crate::attention::tests::{notice, request, summary};
use crate::auth::AuthPolicy;
use crate::config::{Services, Tuning};
use crate::hub::{ClientId, ClientSink, Focus, Hub};
use crate::store::{OwnerId, PushSubscriptionRow, Store};

/// One recorded push: the device, the payload's `state`, and whether it was urgent.
type Sent = (String, String, bool);

/// Records every push, and its whole payload.
#[derive(Default)]
struct Recorder(Mutex<Vec<Sent>>, Mutex<Vec<serde_json::Value>>);

impl PushSender for Recorder {
    fn send<'a>(
        &'a self,
        sub: &'a PushSubscriptionRow,
        payload: &'a [u8],
        urgent: bool,
    ) -> BoxFuture<'a, PushOutcome> {
        let payload: serde_json::Value = serde_json::from_slice(payload).unwrap();
        let state = payload["state"].as_str().unwrap().to_owned();
        self.0
            .lock()
            .unwrap()
            .push((sub.device_id.to_string(), state, urgent));
        self.1.lock().unwrap().push(payload);
        Box::pin(async { PushOutcome::Delivered })
    }
}

impl Recorder {
    fn sent(&self) -> Vec<Sent> {
        self.0.lock().unwrap().clone()
    }

    fn payloads(&self) -> Vec<serde_json::Value> {
        self.1.lock().unwrap().clone()
    }

    fn devices(&self) -> Vec<String> {
        let mut devices: Vec<_> = self.sent().into_iter().map(|(d, _, _)| d).collect();
        devices.sort();
        devices
    }
}

/// A hub with Web Push to `phone` and `laptop`, on a store that outlives it (a restart starts
/// another hub on the same store).
struct Setup {
    store: Store,
    push: Arc<Recorder>,
    hub: Arc<Hub>,
    /// Keeps the clients' outbound queues open.
    queues: Vec<mpsc::Receiver<String>>,
    rev: u64,
}

impl Setup {
    fn new() -> Self {
        let store = Store::open_in_memory().unwrap();
        let push = Arc::new(Recorder::default());
        let hub = start_hub(&store, &push);
        let repo = hub.repo();
        for (n, device) in ["phone", "laptop"].into_iter().enumerate() {
            let device = DeviceId::new(device);
            repo.devices().register(&device, device.as_str()).unwrap();
            let sub = PushSubscriptionRow {
                device_id: device,
                endpoint: format!("https://push.example.com/send/{n}"),
                p256dh: "BPubKey".into(),
                auth: "AuthSecret".into(),
            };
            assert!(repo.push().set(&sub).unwrap());
        }
        Self {
            store,
            push,
            hub,
            queues: Vec::new(),
            rev: 0,
        }
    }

    /// Stops the hub and starts a new one on the same store, as a controller restart does: the
    /// agent daemon's sessions arrive again as first observations.
    async fn restart(&mut self) {
        self.hub.shutdown().await;
        self.hub = start_hub(&self.store, &self.push);
        self.queues.clear();
        self.rev = 0;
    }

    fn set_prefs(&self, p: NotificationsSetPreferencesParams) {
        prefs::set(&self.hub.repo().prefs(), &p).unwrap();
    }

    /// The agent daemon reports a session's summary.
    fn upsert(&mut self, session: &SessionSummary) {
        let mut st = self.hub.lock();
        let sub = match st.global.sub {
            Some(sub) => sub,
            None => {
                let sub = SubscriptionId::generate();
                st.sessions_subscribed(SessionsSubscribeResult {
                    subscription_id: sub,
                    rev: Rev(0),
                    snapshot: None,
                });
                sub
            }
        };
        self.rev += 1;
        st.sessions_events(SessionsEventsParams {
            subscription_id: sub,
            changes: vec![SessionsChange {
                rev: Rev(self.rev),
                event: SessionsEvent::SessionUpserted {
                    session: Box::new(session.clone()),
                },
            }],
        });
    }

    /// A UI connection from `device`, visible and showing no session.
    fn connect(&mut self, device: &str) -> ClientId {
        let (tx, rx) = mpsc::channel(64);
        let (close, _) = watch::channel(None);
        self.queues.push(rx);
        let client = self.hub.add_client(ClientSink::new(tx, close), None);
        self.focus(client, device, true, None);
        client
    }

    fn focus(&self, client: ClientId, device: &str, visible: bool, session: Option<SessionId>) {
        let focus = Focus {
            device_id: DeviceId::new(device),
            session_id: session,
            visible,
        };
        self.hub.set_focus(client, focus);
    }
}

fn start_hub(store: &Store, push: &Arc<Recorder>) -> Arc<Hub> {
    let services = Arc::new(Services {
        store: store.clone(),
        master_key: MasterKey::from_bytes([7; 32]),
        binaries: Err("none".into()),
        server_version: "0.0.0-test".into(),
        controller_id: store.controller_id().unwrap(),
        auth: AuthPolicy {
            mode: AuthMode::DevToken,
            allowed_subjects: vec![],
            allowed_groups: vec![],
        },
        push: Some(PushConfig {
            vapid_public_key: "BTest".into(),
            sender: push.clone(),
        }),
        key_comment: "controlroom@controlroom.example.com".into(),
        agentd_socket: None,
        tuning: Tuning::default(),
    });
    Hub::start(services, OwnerId::local()).unwrap()
}

/// Lets the paused clock run `secs` seconds; every task gets to run first.
async fn pass(secs: u64) {
    tokio::time::sleep(Duration::from_secs(secs)).await;
}

/// Lets every task run without moving the clock meaningfully.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

/// A session waiting for approval `id`.
fn asking(id: &str) -> SessionSummary {
    let mut session = summary();
    session.requests = vec![request(id, true)];
    session
}

fn answered(mut session: SessionSummary) -> SessionSummary {
    for r in &mut session.requests {
        r.state = RequestState::Answered;
    }
    session
}

#[tokio::test(start_paused = true)]
async fn with_nobody_active_a_push_waits_the_delay() {
    let mut s = Setup::new();
    s.upsert(&summary());
    s.upsert(&asking("r1"));
    pass(29).await;
    assert!(s.push.sent().is_empty(), "not before the delay");
    pass(2).await;
    assert_eq!(s.push.devices(), ["laptop", "phone"]);
    assert!(
        s.push
            .sent()
            .iter()
            .all(|(_, state, urgent)| state == "approval" && *urgent)
    );
}

#[tokio::test(start_paused = true)]
async fn without_a_delay_a_push_goes_at_once() {
    let mut s = Setup::new();
    s.set_prefs(NotificationsSetPreferencesParams {
        delay_s: Some(0),
        ..prefs_unchanged()
    });
    s.upsert(&summary());
    s.upsert(&asking("r1"));
    settle().await;
    assert_eq!(s.push.devices(), ["laptop", "phone"]);
}

#[tokio::test(start_paused = true)]
async fn an_answer_within_the_delay_cancels_the_push() {
    let mut s = Setup::new();
    s.upsert(&summary());
    s.upsert(&asking("r1"));
    pass(10).await;
    s.upsert(&answered(asking("r1")));
    pass(60).await;
    assert!(s.push.sent().is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_active_device_holds_pushes_to_every_device_until_it_goes_idle() {
    let mut s = Setup::new();
    let laptop = s.connect("laptop");
    s.hub.record_input(laptop);
    s.upsert(&summary());
    s.upsert(&asking("r1"));
    pass(100).await;
    assert!(
        s.push.sent().is_empty(),
        "the active laptop shows it in-app"
    );
    // Input keeps the laptop active: the idle window (2 min) starts again.
    s.hub.record_input(laptop);
    pass(119).await;
    assert!(s.push.sent().is_empty());
    // Walked away with the approval unanswered: pushed then, urgent, to every device.
    pass(2).await;
    assert_eq!(s.push.devices(), ["laptop", "phone"]);
    assert!(s.push.sent().iter().all(|(_, _, urgent)| *urgent));
    pass(600).await;
    assert_eq!(s.push.sent().len(), 2, "once");
}

#[tokio::test(start_paused = true)]
async fn an_approval_answered_while_held_is_never_pushed() {
    let mut s = Setup::new();
    let laptop = s.connect("laptop");
    s.hub.record_input(laptop);
    s.upsert(&summary());
    s.upsert(&asking("r1"));
    pass(60).await;
    s.upsert(&answered(asking("r1")));
    pass(600).await;
    assert!(s.push.sent().is_empty());
}

#[tokio::test(start_paused = true)]
async fn hiding_or_disconnecting_the_active_client_releases_held_pushes() {
    let mut s = Setup::new();
    let laptop = s.connect("laptop");
    s.hub.record_input(laptop);
    s.upsert(&summary());
    s.upsert(&asking("r1"));
    pass(40).await;
    assert!(s.push.sent().is_empty());
    // The tab is hidden: presence ends at once.
    s.focus(laptop, "laptop", false, None);
    settle().await;
    assert_eq!(s.push.devices(), ["laptop", "phone"]);

    // Back and active; a second approval is held until the connection drops.
    s.focus(laptop, "laptop", true, None);
    s.hub.record_input(laptop);
    s.upsert(&asking("r2"));
    pass(40).await;
    assert_eq!(s.push.sent().len(), 2);
    s.hub.remove_client(laptop);
    settle().await;
    assert_eq!(s.push.sent().len(), 4);
}

#[tokio::test(start_paused = true)]
async fn a_visible_idle_client_is_not_active_and_skips_only_its_session() {
    let mut s = Setup::new();
    let laptop = s.connect("laptop");
    let session = asking("r1");
    // Showing the session, but no input: not active.
    s.focus(laptop, "laptop", true, Some(session.id));
    s.upsert(&summary());
    s.upsert(&session);
    pass(31).await;
    assert_eq!(s.push.devices(), ["phone"], "the laptop shows it");
}

#[tokio::test(start_paused = true)]
async fn holding_can_be_turned_off() {
    let mut s = Setup::new();
    s.set_prefs(NotificationsSetPreferencesParams {
        hold_while_active: Some(false),
        ..prefs_unchanged()
    });
    let laptop = s.connect("laptop");
    s.hub.record_input(laptop);
    s.upsert(&summary());
    s.upsert(&asking("r1"));
    pass(31).await;
    assert_eq!(s.push.devices(), ["laptop", "phone"]);
}

#[tokio::test(start_paused = true)]
async fn a_restart_during_the_delay_still_pushes_a_pending_request() {
    let mut s = Setup::new();
    s.upsert(&summary());
    s.upsert(&asking("r1"));
    pass(20).await;
    s.restart().await;
    // The agent daemon's sessions arrive again after the restart.
    s.upsert(&asking("r1"));
    pass(29).await;
    assert!(s.push.sent().is_empty());
    pass(2).await;
    assert_eq!(s.push.devices(), ["laptop", "phone"]);
}

#[tokio::test(start_paused = true)]
async fn dedup_holds_across_hold_release_and_restart() {
    let mut s = Setup::new();
    s.upsert(&summary());
    s.upsert(&asking("r1"));
    pass(31).await;
    assert_eq!(s.push.sent().len(), 2);
    // After a restart the still pending approval is raised again and held while the laptop is
    // active; released when it hides, it is not sent twice.
    s.restart().await;
    let laptop = s.connect("laptop");
    s.hub.record_input(laptop);
    s.upsert(&asking("r1"));
    pass(60).await;
    s.focus(laptop, "laptop", false, None);
    pass(600).await;
    assert_eq!(s.push.sent().len(), 2);
}

/// Preference params that change nothing.
fn prefs_unchanged() -> NotificationsSetPreferencesParams {
    NotificationsSetPreferencesParams {
        attention: None,
        questions: None,
        turn_end: None,
        agent_notices: None,
        hold_while_active: None,
        delay_s: None,
        idle_s: None,
    }
}

/// C11: a private profile's push carries its profile's name and the attention kind, never the
/// session's name or machine; other profiles' pushes show the session.
#[tokio::test(start_paused = true)]
async fn a_private_profiles_push_carries_no_session_name_or_machine() {
    use controlroom_protocol::agentd::ModelPolicy;

    use crate::store::NewProfile;

    let mut s = Setup::new();
    s.set_prefs(NotificationsSetPreferencesParams {
        delay_s: Some(0),
        ..prefs_unchanged()
    });
    let new = |name: &str, private| NewProfile {
        name: name.into(),
        color: "#000".into(),
        private,
        models: if private {
            ModelPolicy::Only { accounts: vec![] }
        } else {
            ModelPolicy::Any
        },
    };
    let repo = s.hub.repo();
    let books = repo.profiles().insert(new("Book club", true)).unwrap();
    let garden = repo.profiles().insert(new("Garden club", false)).unwrap();
    let session = |id: &str, name: &str, profile| {
        let mut idle = summary();
        idle.id = id.parse().unwrap();
        idle.name = Some(name.into());
        idle.profile_id = Some(profile);
        let mut waiting = idle.clone();
        waiting.requests = vec![request("r1", true)];
        (idle, waiting)
    };
    let (idle, waiting) = session("01HZY3K6W8Q9X2V4N7B5C1D3E7", "Plot twist", books.id);
    s.upsert(&idle);
    s.upsert(&waiting);
    let (idle, waiting) = session("01HZY3K6W8Q9X2V4N7B5C1D3E8", "Tomatoes", garden.id);
    s.upsert(&idle);
    s.upsert(&waiting);
    settle().await;

    let payloads = s.push.payloads();
    assert_eq!(payloads.len(), 4, "two sessions, two devices");
    let private: Vec<_> = payloads
        .iter()
        .filter(|p| p["profile_id"] == serde_json::json!(books.id))
        .collect();
    assert_eq!(private.len(), 2);
    for payload in private {
        assert_eq!(
            *payload,
            serde_json::json!({
                "session_id": "01HZY3K6W8Q9X2V4N7B5C1D3E7", "profile_id": books.id,
                "state": "approval", "url": "#/s/01HZY3K6W8Q9X2V4N7B5C1D3E7",
                "kind": "private", "profile": "Book club",
            })
        );
        assert!(!payload.to_string().contains("Plot twist"));
    }
    let public = payloads
        .iter()
        .find(|p| p["profile_id"] == serde_json::json!(garden.id))
        .unwrap();
    assert_eq!(public["kind"], "session");
    assert_eq!(public["name"], "Tomatoes");

    // An agent notice in the private profile: metadata only, no session name or notice text.
    let mut noticed = summary();
    noticed.id = "01HZY3K6W8Q9X2V4N7B5C1D3E7".parse().unwrap();
    noticed.name = Some("Plot twist".into());
    noticed.profile_id = Some(books.id);
    noticed.notices = vec![notice("n1", NoticeLevel::Error)];
    s.upsert(&noticed);
    settle().await;
    let payloads = s.push.payloads();
    assert_eq!(payloads.len(), 6);
    for payload in &payloads[4..] {
        assert_eq!(
            *payload,
            serde_json::json!({
                "session_id": "01HZY3K6W8Q9X2V4N7B5C1D3E7", "profile_id": books.id,
                "state": "notice", "url": "#/s/01HZY3K6W8Q9X2V4N7B5C1D3E7",
                "kind": "private", "profile": "Book club",
            })
        );
    }
}

/// A session with these open agent notices.
fn noticed(notices: &[(&str, NoticeLevel)]) -> SessionSummary {
    let mut session = summary();
    session.notices = notices
        .iter()
        .map(|(id, level)| notice(id, *level))
        .collect();
    session
}

/// A warning or an error pushes like an approval (delay, presence hold, dedup across a
/// restart), but not urgently, and never carries the notice's text.
#[tokio::test(start_paused = true)]
async fn agent_notices_push_after_the_delay_and_while_nobody_is_active() {
    let mut s = Setup::new();
    let laptop = s.connect("laptop");
    s.hub.record_input(laptop);
    s.upsert(&summary());
    s.upsert(&noticed(&[("n1", NoticeLevel::Warning)]));
    pass(100).await;
    assert!(s.push.sent().is_empty(), "held while the laptop is active");
    s.focus(laptop, "laptop", false, None);
    settle().await;
    assert_eq!(s.push.devices(), ["laptop", "phone"]);
    assert!(
        s.push
            .sent()
            .iter()
            .all(|(_, state, urgent)| state == "notice" && !*urgent)
    );
    assert!(
        s.push
            .payloads()
            .iter()
            .all(|p| !p.to_string().contains("failed unit"))
    );
    // Still open after a restart: raised again, never sent twice.
    s.restart().await;
    s.upsert(&noticed(&[("n1", NoticeLevel::Warning)]));
    pass(600).await;
    assert_eq!(s.push.sent().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_notice_dismissed_within_the_delay_is_never_pushed() {
    let mut s = Setup::new();
    s.upsert(&summary());
    s.upsert(&noticed(&[("n1", NoticeLevel::Error)]));
    pass(10).await;
    s.upsert(&summary());
    pass(600).await;
    assert!(s.push.sent().is_empty());
}

/// Info never pushes; the preference picks warnings and errors (the default), errors only, or
/// none.
#[tokio::test(start_paused = true)]
async fn the_agent_notices_preference_picks_the_levels_that_push() {
    use controlroom_protocol::ui::NoticePush;

    let mut s = Setup::new();
    s.set_prefs(NotificationsSetPreferencesParams {
        delay_s: Some(0),
        ..prefs_unchanged()
    });
    s.upsert(&summary());
    s.upsert(&noticed(&[("i1", NoticeLevel::Info)]));
    settle().await;
    assert!(s.push.sent().is_empty(), "info never pushes");

    s.set_prefs(NotificationsSetPreferencesParams {
        agent_notices: Some(NoticePush::Error),
        ..prefs_unchanged()
    });
    s.upsert(&noticed(&[("w1", NoticeLevel::Warning)]));
    settle().await;
    assert!(s.push.sent().is_empty(), "errors only");
    s.upsert(&noticed(&[
        ("w1", NoticeLevel::Warning),
        ("e1", NoticeLevel::Error),
    ]));
    settle().await;
    assert_eq!(s.push.devices(), ["laptop", "phone"]);

    s.set_prefs(NotificationsSetPreferencesParams {
        agent_notices: Some(NoticePush::Off),
        ..prefs_unchanged()
    });
    s.upsert(&noticed(&[("e2", NoticeLevel::Error)]));
    settle().await;
    assert_eq!(s.push.sent().len(), 2, "off");
    assert_eq!(
        prefs::load(&s.hub.repo().prefs()).unwrap().agent_notices,
        NoticePush::Off
    );
    assert!(
        prefs::check(&NotificationsSetPreferencesParams {
            agent_notices: Some(NoticePush::Unknown),
            ..prefs_unchanged()
        })
        .is_err()
    );
}
