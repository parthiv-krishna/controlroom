//! Schedules end to end: the controller's scheduler, on a manual clock, firing `session.prompt`
//! at a fake agent daemon (which applies op ids once, `if_busy` and closed sessions like the
//! real one).
#![cfg(unix)]

mod support;

use std::path::Path;
use std::time::Duration;

use controlroom_controller::store::{OwnerId, Store, Timing};
use controlroom_controller::test_support::{FakeAgentd, UiDriver, session_summary};
use controlroom_controller::{Clock, Controller, ManualClock};
use controlroom_protocol::UnixMillis;
use controlroom_protocol::ids::{MachineId, SessionId};
use controlroom_protocol::session::{SessionState, WorkspaceSpec};
use controlroom_protocol::ui::SchedulesUpdateParams;
use serde_json::{Value, json};
use support::{config, ui};

/// Monday 2025-01-06 08:00 UTC.
const T0: i64 = 1_736_150_400_000;
const MINUTE: Duration = Duration::from_secs(60);
const MIN: i64 = 60_000;

fn start(store: &Path, clock: &Clock, agentd: &FakeAgentd) -> Controller {
    let mut config = config(store, None, None);
    config.tuning.clock = clock.clone();
    config.agentd_socket = Some(agentd.socket().to_owned());
    Controller::start(config).unwrap()
}

fn manual() -> (Clock, ManualClock) {
    Clock::manual(UnixMillis(T0))
}

/// A global session on the agent daemon, in `state`.
fn session(agentd: &FakeAgentd, state: SessionState) -> SessionId {
    let id = SessionId::generate();
    let mut s = session_summary(
        id,
        &[WorkspaceSpec {
            machine_id: MachineId::generate(),
            path: "~/src".into(),
        }],
    );
    s.state = state;
    agentd.upsert_session(s);
    id
}

/// Sets a session's state on the agent daemon.
fn set_state(agentd: &FakeAgentd, id: SessionId, state: SessionState) {
    let mut s = agentd.session(id).unwrap();
    s.state = state;
    agentd.upsert_session(s);
}

/// Waits until the controller's feed has the session in `state`.
async fn wait_state(ui: &mut UiDriver, id: SessionId, state: &str) {
    ui.wait(&format!("the session is {state}"), |m| {
        m.sessions
            .get(&id.to_string())
            .is_some_and(|s| s["state"] == state)
    })
    .await;
}

/// Creates a schedule every minute on `session`; returns its id.
async fn every_minute(ui: &mut UiDriver, session: SessionId, name: &str, extra: Value) -> String {
    let mut params = json!({"session_id": session, "name": name,
        "spec": {"kind": "interval", "every_s": 60}, "timezone": "Europe/Berlin",
        "message": format!("{name} says hi")});
    for (k, v) in extra.as_object().into_iter().flatten() {
        params[k] = v.clone();
    }
    let s = ui.ok("schedules.create", params).await;
    s["id"].as_str().unwrap().to_owned()
}

/// Waits until the schedule's newest run is at `at` with `outcome`, with nothing pending.
async fn wait_run(ui: &mut UiDriver, id: &str, at: i64, outcome: &str) -> Value {
    let what = format!("schedule run at +{} min: {outcome}", (at - T0) / MIN);
    ui.wait(&what, |m| {
        let s = &m.schedules[id];
        s["recent_runs"][0]["at"] == at
            && s["recent_runs"][0]["outcome"] == outcome
            && s["pending_run"].is_null()
    })
    .await;
    ui.mirror.schedules[id].clone()
}

/// The prompts the agent daemon applied whose text contains `needle`.
fn prompts(agentd: &FakeAgentd, needle: &str) -> Vec<Value> {
    agentd
        .prompts()
        .into_iter()
        .filter(|p| p["content"].to_string().contains(needle))
        .collect()
}

/// A due time fires once: a controller that stopped after delivering but before recording
/// delivers it again with the same op id, and the agent daemon doesn't prompt twice. Also
/// `run_now`, the prompt's origin and the advertised capability.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_due_time_fires_once_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("controller.sqlite");
    let (clock, time) = manual();
    let agentd = FakeAgentd::start().await;
    let session = session(&agentd, SessionState::Idle);

    let first = start(&store, &clock, &agentd);
    let mut a = ui(&first, "laptop").await;
    let hello = a
        .ok(
            "hello",
            json!({"protocol_version": {"major": 1, "minor": 0}}),
        )
        .await;
    assert_eq!(hello["capabilities"], json!(["schedules"]), "{hello}");
    wait_state(&mut a, session, "idle").await;
    let id = every_minute(&mut a, session, "inbox", json!({})).await;
    assert_eq!(a.mirror.schedules[&id]["next_run"], T0 + MIN);

    time.advance(MINUTE);
    let s = wait_run(&mut a, &id, T0 + MIN, "fired").await;
    assert_eq!(s["next_run"], T0 + 2 * MIN);
    let sent = prompts(&agentd, "inbox says hi");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["op_id"], format!("{id}:{}", T0 + MIN));
    assert_eq!(
        sent[0]["origin"],
        json!({"kind": "schedule", "schedule_id": id, "name": "inbox"})
    );
    drop(a);
    first.shutdown().await;

    // Rewind to the moment after the claim, before the outcome was recorded.
    let owner = OwnerId::local();
    let rewound = Store::open(&store)
        .unwrap()
        .owner(&owner)
        .schedules()
        .update(
            &SchedulesUpdateParams {
                schedule_id: id.parse().unwrap(),
                name: None,
                spec: None,
                timezone: None,
                message: None,
                enabled: None,
                if_busy: None,
                missed: None,
            },
            Some(Timing {
                next_run: Some(UnixMillis(T0 + 2 * MIN)),
                pending_run: Some(UnixMillis(T0 + MIN)),
            }),
        )
        .unwrap();
    assert!(rewound);

    let second = start(&store, &clock, &agentd);
    let mut b = ui(&second, "laptop").await;
    let s = wait_run(&mut b, &id, T0 + MIN, "fired").await;
    assert_eq!(s["recent_runs"].as_array().unwrap().len(), 1, "{s}");
    assert_eq!(
        prompts(&agentd, "inbox says hi").len(),
        1,
        "the retried due time didn't prompt again"
    );
    // Run now with a new message (which doesn't move the next run).
    let edited = b
        .ok(
            "schedules.update",
            json!({"schedule_id": id, "message": "second check"}),
        )
        .await;
    assert_eq!(edited["next_run"], T0 + 2 * MIN);
    time.advance(Duration::from_secs(5));
    let now = b.ok("schedules.run_now", json!({"schedule_id": id})).await;
    assert_eq!(now["recent_runs"][0]["at"], T0 + MIN + 5_000, "{now}");
    assert_eq!(now["recent_runs"][0]["outcome"], "fired");
    assert_eq!(now["recent_runs"][0]["detail"], "run now");
    assert_eq!(prompts(&agentd, "second check").len(), 1);
    second.shutdown().await;
}

/// `if_busy`: while a run is active, `skip` drops the message and `queue` queues it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_busy_session_skips_or_queues() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, time) = manual();
    let agentd = FakeAgentd::start().await;
    let session = session(&agentd, SessionState::Working);
    let c = start(&dir.path().join("c.sqlite"), &clock, &agentd);
    let mut a = ui(&c, "laptop").await;
    wait_state(&mut a, session, "working").await;
    let skip = every_minute(&mut a, session, "skipper", json!({"if_busy": "skip"})).await;
    let queue = every_minute(&mut a, session, "queuer", json!({})).await;

    time.advance(MINUTE);
    let s = wait_run(&mut a, &skip, T0 + MIN, "skipped").await;
    assert_eq!(s["recent_runs"][0]["detail"], "the session was busy");
    wait_run(&mut a, &queue, T0 + MIN, "queued").await;
    assert_eq!(prompts(&agentd, "queuer says hi")[0]["if_busy"], "queue");
    assert_eq!(prompts(&agentd, "skipper says hi")[0]["if_busy"], "skip");
    c.shutdown().await;
}

/// Due times that pass while the controller is down: `skip` records them as missed, `run_once`
/// sends the message once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn downtime_skips_or_runs_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("controller.sqlite");
    let (clock, time) = manual();
    let agentd = FakeAgentd::start().await;
    let session = session(&agentd, SessionState::Idle);

    let first = start(&store, &clock, &agentd);
    let mut a = ui(&first, "laptop").await;
    let skip = every_minute(&mut a, session, "skipper", json!({})).await;
    let once = every_minute(&mut a, session, "catcher", json!({"missed": "run_once"})).await;
    drop(a);
    first.shutdown().await;

    // Down for three due times (+1, +2 and +3 min).
    time.advance(Duration::from_secs(210));
    let second = start(&store, &clock, &agentd);
    let mut b = ui(&second, "laptop").await;
    let s = wait_run(&mut b, &skip, T0 + MIN, "missed").await;
    let run = &s["recent_runs"][0];
    assert_eq!(
        (run["count"].clone(), run["until"].clone()),
        (json!(3), json!(T0 + 3 * MIN))
    );
    assert_eq!(run["detail"], "the server was down");
    assert_eq!(s["next_run"], T0 + 4 * MIN);
    let s = wait_run(&mut b, &once, T0 + 3 * MIN, "fired").await;
    assert!(
        s["recent_runs"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("3 runs missed"),
        "{s}"
    );
    assert_eq!(s["recent_runs"].as_array().unwrap().len(), 1);
    assert_eq!(s["next_run"], T0 + 4 * MIN);
    assert_eq!(prompts(&agentd, "catcher says hi").len(), 1);
    assert!(prompts(&agentd, "skipper says hi").is_empty());
    second.shutdown().await;
}

/// An unreachable agent daemon: the due time stays pending and is delivered when it is back;
/// if it is still away at the next due time, the run is missed. `run_now` meanwhile is an
/// error and records nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_agent_daemon_is_retried_until_the_next_due_time() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, time) = manual();
    let agentd = FakeAgentd::start().await;
    let session = session(&agentd, SessionState::Idle);
    let c = start(&dir.path().join("c.sqlite"), &clock, &agentd);
    let mut a = ui(&c, "laptop").await;
    agentd.wait_connected(1).await;
    let id = every_minute(&mut a, session, "inbox", json!({})).await;

    // Away at +1 min, back before +2 min: delivered late.
    agentd.set_down(true);
    a.err("schedules.run_now", json!({"schedule_id": id}), "timeout")
        .await;
    assert_eq!(a.mirror.schedules[&id]["recent_runs"], json!([]));
    time.advance(MINUTE);
    a.wait("the run is pending", |m| {
        m.schedules[&id]["pending_run"] == T0 + MIN
    })
    .await;
    agentd.set_down(false);
    wait_run(&mut a, &id, T0 + MIN, "fired").await;
    assert_eq!(prompts(&agentd, "inbox says hi").len(), 1);

    // Away from +2 min to after +3 min: +2 is missed, +3 is delivered.
    agentd.set_down(true);
    time.advance(MINUTE);
    a.wait("the run is pending", |m| {
        m.schedules[&id]["pending_run"] == T0 + 2 * MIN
    })
    .await;
    time.advance(MINUTE);
    a.wait("the next run is pending", |m| {
        m.schedules[&id]["pending_run"] == T0 + 3 * MIN
    })
    .await;
    agentd.set_down(false);
    let s = wait_run(&mut a, &id, T0 + 3 * MIN, "fired").await;
    let runs: Vec<_> = s["recent_runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["at"].as_i64().unwrap(),
                r["outcome"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        runs,
        [
            (T0 + 3 * MIN, "fired".to_owned()),
            (T0 + 2 * MIN, "missed".to_owned()),
            (T0 + MIN, "fired".to_owned())
        ]
    );
    assert_eq!(s["recent_runs"][1]["detail"], "its daemon was unreachable");
    assert_eq!(prompts(&agentd, "inbox says hi").len(), 2);
    c.shutdown().await;
}

/// Closing a session disables its schedules (they can't be enabled while it is closed);
/// deleting it does too. Invalid specs are refused with a reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closed_and_deleted_sessions_disable_their_schedules() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, _time) = manual();
    let agentd = FakeAgentd::start().await;
    let session = session(&agentd, SessionState::Idle);
    let c = start(&dir.path().join("c.sqlite"), &clock, &agentd);
    let mut a = ui(&c, "laptop").await;
    wait_state(&mut a, session, "idle").await;
    let id = every_minute(&mut a, session, "inbox", json!({})).await;

    set_state(&agentd, session, SessionState::Closed);
    a.wait("the schedule is disabled", |m| {
        m.schedules[&id]["enabled"] == false && m.schedules[&id]["next_run"].is_null()
    })
    .await;
    a.err(
        "schedules.update",
        json!({"schedule_id": id, "enabled": true}),
        "invalid_params",
    )
    .await;
    a.err(
        "schedules.create",
        json!({"session_id": session, "name": "x",
               "spec": {"kind": "cron", "expr": "0 9 * * *"}, "timezone": "UTC", "message": "m"}),
        "invalid_params",
    )
    .await;

    set_state(&agentd, session, SessionState::Dormant);
    wait_state(&mut a, session, "dormant").await;
    let enabled = a
        .ok(
            "schedules.update",
            json!({"schedule_id": id, "enabled": true}),
        )
        .await;
    assert_eq!(enabled["next_run"], T0 + MIN);

    agentd.remove_session(session);
    a.wait("deleting the session disables the schedule", |m| {
        m.schedules[&id]["enabled"] == false
    })
    .await;

    let bad = a
        .err(
            "schedules.update",
            json!({"schedule_id": id, "spec": {"kind": "cron", "expr": "0 25 * * *"}}),
            "invalid_params",
        )
        .await;
    assert!(bad.to_string().contains("invalid cron"), "{bad}");
    a.err(
        "schedules.update",
        json!({"schedule_id": id, "timezone": "Mars/Olympus"}),
        "invalid_params",
    )
    .await;
    c.shutdown().await;
}
