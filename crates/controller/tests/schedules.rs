//! Schedules end to end: the controller's scheduler, on a manual clock, firing through a real
//! `controlroomd` with the fake agent over in-process SSH.
#![cfg(unix)]

mod support;

use std::path::Path;
use std::time::Duration;

use controlroom_controller::store::{OwnerId, Store, Timing};
use controlroom_controller::test_support::{
    Mirror, RemoteHost, TIMEOUT, UiDriver, bundled_daemons, gated, text,
};
use controlroom_controller::{Clock, Controller, Identity, ManualClock};
use controlroom_protocol::UnixMillis;
use controlroom_protocol::ui::SchedulesUpdateParams;
use serde_json::{Value, json};
use support::relay::Relay;
use support::{add_machine, add_machine_at, config, create_session, ui};

/// Monday 2025-01-06 08:00 UTC.
const T0: i64 = 1_736_150_400_000;
const MINUTE: Duration = Duration::from_secs(60);
const MIN: i64 = 60_000;

fn start(store: &Path, bundled: &Path, clock: &Clock) -> Controller {
    let mut config = config(store, Some(bundled), None);
    config.tuning.clock = clock.clone();
    Controller::start(config).unwrap()
}

fn manual() -> (Clock, ManualClock) {
    Clock::manual(UnixMillis(T0))
}

/// Creates a schedule every minute on `session`; returns its id.
async fn every_minute(
    ui: &mut UiDriver,
    machine: &str,
    session: &str,
    name: &str,
    extra: Value,
) -> String {
    let mut params = json!({"machine_id": machine, "session_id": session, "name": name,
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

/// Waits until the session's timeline has `n` user messages containing `needle`.
async fn wait_messages(ui: &mut UiDriver, machine: &str, session: &str, needle: &str, n: usize) {
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        let found = user_messages(ui, machine, session, needle).await;
        if found == n {
            return;
        }
        assert!(
            found < n && std::time::Instant::now() < deadline,
            "{found} messages \"{needle}\", expected {n}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The session's timeline backlog: one upsert per item.
async fn timeline(ui: &mut UiDriver, machine: &str, session: &str) -> Vec<Value> {
    let sub = ui
        .ok(
            "session.subscribe",
            json!({"machine_id": machine, "id": session, "since_seq": 0}),
        )
        .await;
    let backlog = ui
        .notification("the backlog", |n| {
            n["method"] == "session.events"
                && n["params"]["subscription_id"] == sub["subscription_id"]
        })
        .await;
    ui.ok(
        "unsubscribe",
        json!({"subscription_id": sub["subscription_id"]}),
    )
    .await;
    backlog["params"]["ops"].as_array().unwrap().clone()
}

/// The user messages in the session's timeline that contain `needle`.
async fn user_messages(ui: &mut UiDriver, machine: &str, session: &str, needle: &str) -> usize {
    let mut ids: Vec<String> = timeline(ui, machine, session)
        .await
        .iter()
        .filter(|op| {
            op["item"]["kind"] == "message"
                && op["item"]["role"] == "user"
                && op["item"].to_string().contains(needle)
        })
        .map(|op| op["item"]["id"].to_string())
        .collect();
    ids.sort();
    ids.dedup();
    ids.len()
}

fn replies(n: usize) -> Value {
    let texts: Vec<String> = (0..n).map(|i| format!("reply {i}")).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    controlroom_controller::test_support::texts(&refs)
}

/// A due time fires once: a controller that stopped after delivering but before recording
/// delivers it again with the same op id, and the daemon doesn't prompt twice. Also `run_now`
/// and the advertised capability.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_due_time_fires_once_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("controller.sqlite");
    let bundled = bundled_daemons();
    let (clock, time) = manual();
    let mut host = RemoteHost::start().await;
    host.agent("fake", replies(4));

    let first = start(&store, bundled.path(), &clock);
    let principal = first.resolve(&Identity::DevToken).unwrap();
    let mut a = UiDriver::new(first.open_trusted_client(principal).unwrap());
    let hello = a.hello("laptop", "Laptop").await;
    assert_eq!(hello["capabilities"], json!(["schedules"]), "{hello}");
    a.subscribe_state().await;
    let machine = add_machine(&mut a, &host, "devbox").await;
    let session = create_session(&mut a, &machine, &host, json!({})).await;
    let id = every_minute(&mut a, &machine, &session, "inbox", json!({})).await;
    assert_eq!(a.mirror.schedules[&id]["next_run"], T0 + MIN);

    time.advance(MINUTE);
    let s = wait_run(&mut a, &id, T0 + MIN, "fired").await;
    assert_eq!(s["next_run"], T0 + 2 * MIN);
    wait_messages(&mut a, &machine, &session, "inbox says hi", 1).await;
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

    let second = start(&store, bundled.path(), &clock);
    let mut b = ui(&second, "laptop").await;
    let s = wait_run(&mut b, &id, T0 + MIN, "fired").await;
    assert_eq!(s["recent_runs"].as_array().unwrap().len(), 1, "{s}");
    // Run now with a new message (which doesn't move the next run): it reaches the agent after
    // anything the retry could have sent, and the retry sent nothing.
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
    wait_messages(&mut b, &machine, &session, "second check", 1).await;
    assert_eq!(
        user_messages(&mut b, &machine, &session, "inbox says hi").await,
        1,
        "the retried due time didn't prompt again"
    );
    // Both runs show the schedule as their trigger (the runtime maps the prompt's origin).
    let triggers: Vec<Value> = timeline(&mut b, &machine, &session)
        .await
        .iter()
        .filter(|op| op["item"]["kind"] == "run")
        .map(|op| op["item"]["trigger"].clone())
        .collect();
    assert_eq!(triggers.len(), 2, "{triggers:?}");
    for trigger in &triggers {
        assert_eq!(trigger["kind"], "schedule", "{trigger}");
        assert_eq!(trigger["schedule_id"], json!(id));
        assert_eq!(trigger["name"], "inbox");
    }
    second.shutdown().await;
}

/// `if_busy`: while a run is active, `skip` drops the message and `queue` queues it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_busy_session_skips_or_queues() {
    let dir = tempfile::tempdir().unwrap();
    let bundled = bundled_daemons();
    let (clock, time) = manual();
    let mut host = RemoteHost::start().await;
    host.agent(
        "fake",
        json!({"replies": [gated("working ", "hold", "done"),
            {"content": [{"type": "text", "text": "queued reply"}]}]}),
    );
    let c = start(&dir.path().join("c.sqlite"), bundled.path(), &clock);
    let mut a = ui(&c, "laptop").await;
    let machine = add_machine(&mut a, &host, "devbox").await;
    let session = create_session(
        &mut a,
        &machine,
        &host,
        json!({"prompt": {"content": text("work"), "origin": {"kind": "user", "device": null}}}),
    )
    .await;
    host.wait_gate("hold").await;
    a.wait("the session is working", |m| {
        m.session(&session)["state"] == "working"
    })
    .await;
    let skip = every_minute(
        &mut a,
        &machine,
        &session,
        "skipper",
        json!({"if_busy": "skip"}),
    )
    .await;
    let queue = every_minute(&mut a, &machine, &session, "queuer", json!({})).await;

    time.advance(MINUTE);
    let s = wait_run(&mut a, &skip, T0 + MIN, "skipped").await;
    assert_eq!(s["recent_runs"][0]["detail"], "the session was busy");
    wait_run(&mut a, &queue, T0 + MIN, "queued").await;

    host.release("hold");
    wait_messages(&mut a, &machine, &session, "queuer says hi", 1).await;
    assert_eq!(
        user_messages(&mut a, &machine, &session, "skipper says hi").await,
        0
    );
    c.shutdown().await;
}

/// Due times that pass while the controller is down: `skip` records them as missed, `run_once`
/// sends the message once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn downtime_skips_or_runs_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("controller.sqlite");
    let bundled = bundled_daemons();
    let (clock, time) = manual();
    let mut host = RemoteHost::start().await;
    host.agent("fake", replies(3));

    let first = start(&store, bundled.path(), &clock);
    let mut a = ui(&first, "laptop").await;
    let machine = add_machine(&mut a, &host, "devbox").await;
    let session = create_session(&mut a, &machine, &host, json!({})).await;
    let skip = every_minute(&mut a, &machine, &session, "skipper", json!({})).await;
    let once = every_minute(
        &mut a,
        &machine,
        &session,
        "catcher",
        json!({"missed": "run_once"}),
    )
    .await;
    drop(a);
    first.shutdown().await;

    // Down for three due times (+1, +2 and +3 min).
    time.advance(Duration::from_secs(210));
    let second = start(&store, bundled.path(), &clock);
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
    wait_messages(&mut b, &machine, &session, "catcher says hi", 1).await;
    assert_eq!(
        user_messages(&mut b, &machine, &session, "skipper says hi").await,
        0
    );
    second.shutdown().await;
}

/// An unreachable machine: the due time stays pending and is delivered when the machine is
/// back; if it is still away at the next due time, the run is missed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_machine_is_retried_until_the_next_due_time() {
    let dir = tempfile::tempdir().unwrap();
    let bundled = bundled_daemons();
    let (clock, time) = manual();
    let mut host = RemoteHost::start().await;
    host.agent("fake", replies(3));
    let relay = Relay::start(host.port()).await;
    let c = start(&dir.path().join("c.sqlite"), bundled.path(), &clock);
    let mut a = ui(&c, "laptop").await;
    let machine = add_machine_at(&mut a, &host, "devbox", relay.port).await;
    let session = create_session(&mut a, &machine, &host, json!({})).await;
    let id = every_minute(&mut a, &machine, &session, "inbox", json!({})).await;
    let away = |m: &Mirror| m.machine(&machine)["connectivity"]["state"] != "connected";

    // Away at +1 min, back before +2 min: delivered late.
    relay.down();
    a.wait("the machine is away", away).await;
    time.advance(MINUTE);
    a.wait("the run is pending", |m| {
        m.schedules[&id]["pending_run"] == T0 + MIN
    })
    .await;
    relay.up();
    wait_run(&mut a, &id, T0 + MIN, "fired").await;
    wait_messages(&mut a, &machine, &session, "inbox says hi", 1).await;

    // Away from +2 min to after +3 min: +2 is missed, +3 is delivered.
    relay.down();
    a.wait("the machine is away again", away).await;
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
    relay.up();
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
    assert_eq!(s["recent_runs"][1]["detail"], "the machine was unreachable");
    wait_messages(&mut a, &machine, &session, "inbox says hi", 2).await;
    c.shutdown().await;
}

/// Closing a session disables its schedules (they can't be enabled while it is closed);
/// deleting it does too. `run_now` on an unreachable machine is an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closed_and_deleted_sessions_disable_their_schedules() {
    let dir = tempfile::tempdir().unwrap();
    let bundled = bundled_daemons();
    let (clock, _time) = manual();
    let mut host = RemoteHost::start().await;
    host.agent("fake", replies(1));
    let c = start(&dir.path().join("c.sqlite"), bundled.path(), &clock);
    let mut a = ui(&c, "laptop").await;
    let machine = add_machine(&mut a, &host, "devbox").await;
    let session = create_session(&mut a, &machine, &host, json!({})).await;
    let id = every_minute(&mut a, &machine, &session, "inbox", json!({})).await;
    let op = controlroom_controller::test_support::op;

    a.ok(
        "session.close",
        json!({"machine_id": machine, "op_id": op(), "id": session}),
    )
    .await;
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
        json!({"machine_id": machine, "session_id": session, "name": "x",
               "spec": {"kind": "cron", "expr": "0 9 * * *"}, "timezone": "UTC", "message": "m"}),
        "invalid_params",
    )
    .await;

    a.ok(
        "session.reopen",
        json!({"machine_id": machine, "op_id": op(), "id": session}),
    )
    .await;
    a.wait("the session is open", |m| {
        m.session(&session)["state"] != "closed"
    })
    .await;
    let enabled = a
        .ok(
            "schedules.update",
            json!({"schedule_id": id, "enabled": true}),
        )
        .await;
    assert_eq!(enabled["next_run"], T0 + MIN);

    a.ok(
        "session.delete",
        json!({"machine_id": machine, "op_id": op(), "id": session}),
    )
    .await;
    a.wait("deleting the session disables the schedule", |m| {
        m.schedules[&id]["enabled"] == false
    })
    .await;

    // Invalid specs are refused with a reason.
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

/// Removing the machine disables its schedules; `run_now` without a connection is an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_now_needs_the_machine_and_removal_disables() {
    let dir = tempfile::tempdir().unwrap();
    let bundled = bundled_daemons();
    let (clock, _time) = manual();
    let mut host = RemoteHost::start().await;
    host.agent("fake", replies(1));
    let relay = Relay::start(host.port()).await;
    let c = start(&dir.path().join("c.sqlite"), bundled.path(), &clock);
    let mut a = ui(&c, "laptop").await;
    let machine = add_machine_at(&mut a, &host, "devbox", relay.port).await;
    let session = create_session(&mut a, &machine, &host, json!({})).await;
    let id = every_minute(&mut a, &machine, &session, "inbox", json!({})).await;

    relay.down();
    a.wait("the machine is away", |m| {
        m.machine(&machine)["connectivity"]["state"] != "connected"
    })
    .await;
    a.err(
        "schedules.run_now",
        json!({"schedule_id": id}),
        "machine_unavailable",
    )
    .await;
    assert_eq!(a.mirror.schedules[&id]["recent_runs"], json!([]));

    a.ok("machines.remove", json!({"machine_id": machine}))
        .await;
    a.wait("the schedule is disabled", |m| {
        m.schedules[&id]["enabled"] == false
    })
    .await;
    c.shutdown().await;
}
