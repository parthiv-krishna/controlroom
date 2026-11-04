//! Clients: the handshake and errors, the single-controller rule, subscriptions resuming from
//! `since_seq`/`since_rev`, and `ctl` against a real serve.

use std::io::{BufRead, BufReader};
use std::process::Stdio;

use serde_json::{Value, json};

use crate::controller::CONTROLLER;
use crate::harness::*;

#[test]
fn hello_required_and_errors() {
    let env = Env::with(json!({}));
    let raw = Client::connect(&env.socket());
    raw.call_err("agents.list", json!({}), "hello_required");
    raw.call_err(
        "hello",
        json!({"protocol_version": {"major": 9, "minor": 0}, "client_name": "x", "role": {"kind": "debug"}}),
        "incompatible_version",
    );
    raw.send_raw("not json");
    raw.send_raw(r#"{"jsonrpc":"2.0","id":42,"method":7}"#);
    let hello = raw.hello_debug();
    assert_eq!(hello["protocol_version"]["major"], 1);
    assert!(hello["daemon_id"].is_string());
    // The bad lines failed on their own; the one with a readable id got its answer.
    raw.call_err(
        "hello",
        hello_params(json!({"kind": "debug"})),
        "invalid_request",
    );

    raw.call_err(
        "session.create",
        json!({"op_id": op(), "workspaces": [
            {"machine_id": ulid::Ulid::generate().to_string(), "path": env.work()}
        ]}),
        "machine_unavailable",
    );
    raw.call_err("session.nope", json!({}), "method_not_found");
    raw.call_err("session.prompt", json!({"id": 3}), "invalid_params");
    raw.call_err(
        "sandbox.revoke",
        json!({"op_id": op(), "id": ulid::Ulid::generate().to_string(), "grant_id": ulid::Ulid::generate().to_string()}),
        "unsupported",
    );
    raw.call_err(
        "job.list",
        json!({"id": ulid::Ulid::generate().to_string()}),
        "unknown_session",
    );
    raw.call_err(
        "unsubscribe",
        json!({"subscription_id": ulid::Ulid::generate().to_string()}),
        "not_found",
    );

    let agents = raw.call("agents.list", json!({}));
    let fake = agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["agent"] == "pi")
        .unwrap()
        .clone();
    assert_eq!(
        (fake["status"].as_str(), fake["driver"].as_str()),
        (Some("ok"), Some("pi"))
    );
    // Listings are the hosts' (through the controller), not the agent daemon's.
    raw.call_err("fs.list", json!({"path": "~"}), "method_not_found");
    let settings = raw.call("settings.set", json!({"op_id": op(), "idle_timeout_h": 12}));
    assert_eq!(settings["idle_timeout_h"], 12);
    assert_eq!(raw.call("settings.get", json!({}))["idle_timeout_h"], 12);
    let report = raw.call("doctor.report", json!({}));
    assert!(report["daemon_version"].is_string());
}

fn controller(id: &str) -> Value {
    hello_params(json!({"kind": "controller", "controller_id": id}))
}

#[test]
fn one_controller_per_daemon() {
    let env = Env::with(json!({}));
    // The fake controller is connected: another controller is refused.
    let other = Client::connect(&env.socket());
    let err = other.call_err(
        "hello",
        controller(&ulid::Ulid::generate().to_string()),
        "controller_conflict",
    );
    assert_eq!(err["data"]["controller_id"], CONTROLLER);
    // Debug clients connect alongside, but can't carry host requests.
    let debug = env.client();
    debug.call("settings.get", json!({}));
    debug.call_err("hostops.subscribe", json!({}), "unsupported");
    // The same controller reconnecting replaces the old connection.
    let again = Client::connect(&env.socket());
    again.call("hello", controller(CONTROLLER));
    env.controller().wait_closed();
    again.call("settings.get", json!({}));
    again.call("hostops.subscribe", json!({}));
}

#[test]
fn subscriptions_resume_from_since_seq_and_since_rev() {
    let env = Env::with(texts(&["One.", "Two."]));
    let client = env.client();
    let machine = client.watch_sessions();
    let id = client.create(&env.workspaces());
    client.wait_idle(&id);
    client.prompt(&id, "first");
    client.wait_session(&id, "the first turn", |s| s["last_outcome"] == "completed");

    let (first, ops) = client.session_ops(&id, 0);
    let head = first["head_seq"].as_u64().unwrap();
    assert!(
        ops.iter()
            .any(|op| op["item"]["content"][0]["text"] == "One.")
    );
    let rev = client.call("sessions.subscribe", json!({"since_rev": machine["rev"]}));
    assert!(
        rev["snapshot"].is_null(),
        "a known rev resumes without a snapshot"
    );

    let later = env.client();
    later.watch_sessions();
    later.prompt(&id, "second");
    later.wait_session(&id, "the second turn", |s| {
        s["state"] == "idle" && s["preview"].as_str().unwrap().contains("Two")
    });
    let (_, ops) = later.session_ops(&id, head);
    let texts: Vec<&str> = ops
        .iter()
        .filter_map(|op| op["item"]["content"][0]["text"].as_str())
        .collect();
    assert_eq!(
        texts,
        ["second", "Two."],
        "only items changed after since_seq"
    );

    // Raw frames only for subscriptions that ask for them.
    let raw = later.call(
        "session.subscribe",
        json!({"id": id, "since_seq": head, "raw_since": head}),
    );
    let events = later.wait_notification("raw frames", |n| {
        n["params"]["subscription_id"] == raw["subscription_id"]
    });
    assert!(!events["params"]["raw"].as_array().unwrap().is_empty());
    let unsub = later.call(
        "unsubscribe",
        json!({"subscription_id": raw["subscription_id"]}),
    );
    assert_eq!(unsub, json!({}));
}

#[test]
fn ctl_round_trip() {
    let env = Env::with(texts(&["From ctl."]));
    let ctl = |args: &[&str]| {
        let out = env
            .command()
            .arg("ctl")
            .arg("--state-dir")
            .arg(env.state())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "ctl {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let workspace = format!(
        "{}:{}",
        env.workspaces()[0]["machine_id"].as_str().unwrap(),
        env.work()
    );
    let created = ctl(&["create", "--workspace", &workspace, "--sandbox", "off"]);
    let summary: Value = serde_json::from_str(&created).unwrap();
    let id = summary["id"].as_str().unwrap().to_owned();
    let watcher = env.client();
    watcher.watch_sessions();
    watcher.wait_idle(&id);
    ctl(&["prompt", &id, "hello", "ctl"]);
    watcher.wait_session(&id, "the reply", |s| {
        s["state"] == "idle" && s["preview"].as_str().unwrap().contains("From ctl")
    });
    assert_eq!(env.prompts_sent(&id), ["hello ctl"]);
    let agents: Value = serde_json::from_str(&ctl(&["call", "agents.list"])).unwrap();
    assert!(
        agents["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["agent"] == "pi")
    );

    // `ctl watch` renders the timeline until the daemon goes away.
    let mut watch = env
        .command()
        .arg("ctl")
        .arg("--state-dir")
        .arg(env.state())
        .args(["watch", &id])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut rendered = BufReader::new(watch.stdout.take().unwrap()).lines();
    let mut seen = Vec::new();
    while !seen.iter().any(|l: &String| l.contains("From ctl")) {
        seen.push(rendered.next().expect("ctl watch output").unwrap());
    }
    assert!(seen.iter().any(|l| l == "user: hello ctl"), "{seen:?}");
    let _ = watch.kill();
    let _ = watch.wait();
}

/// Requests run concurrently: an `unsubscribe` sent right after a subscribe result must find
/// the subscription. The daemon used to register it only after queuing the result, which lost
/// the race under heavy load (seen once in a flake check). The fix (register, then answer) is
/// correct by construction; this loop is a stress check, not a deterministic reproduction.
#[test]
fn unsubscribe_right_after_subscribe_always_finds_it() {
    let env = Env::with(json!({}));
    let client = env.client();
    let id = client.create(&env.workspaces());
    for _ in 0..200 {
        let feed = client.call("sessions.subscribe", json!({"since_rev": null}));
        client.call(
            "unsubscribe",
            json!({"subscription_id": feed["subscription_id"]}),
        );
        let session = client.call("session.subscribe", json!({"id": id, "since_seq": 0}));
        client.call(
            "unsubscribe",
            json!({"subscription_id": session["subscription_id"]}),
        );
    }
}
