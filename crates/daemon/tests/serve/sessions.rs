//! Sessions: create, prompt and idle; op replay; options and context usage; start fresh;
//! close, reopen and delete.

use serde_json::{Value, json};

use crate::harness::*;

#[test]
fn create_prompt_reply_idle() {
    let env = Env::with(texts(&["Hello there."]));
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    assert_eq!(client.prompt(&id, "hi")["outcome"], "sent");
    let summary = client.wait_session(&id, "idle after the reply", |s| {
        s["state"] == "idle" && s["last_turn_end_seq"].as_u64() > Some(0)
    });
    assert_eq!(summary["last_outcome"], "completed");
    assert!(summary["preview"].as_str().unwrap().contains("Hello there"));
    assert_eq!(summary["unread"], true);
    assert_eq!(env.prompts_sent(&id), ["hi"]);

    // The timeline holds the prompt and the reply.
    let (_, ops) = client.session_ops(&id, 0);
    let texts: Vec<String> = ops
        .iter()
        .filter(|op| op["item"]["kind"] == "message")
        .map(|op| {
            op["item"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_owned()
        })
        .collect();
    assert_eq!(texts, ["hi", "Hello there."]);

    let cursor = client.call(
        "session.mark_read",
        json!({"op_id": op(), "id": id, "seq": summary["last_turn_end_seq"]}),
    );
    assert_eq!(cursor["unread"], false);
}

#[test]
fn create_with_a_first_prompt_and_a_detail_only_subscription() {
    let env = Env::with(texts(&["Started on it."]));
    let client = env.client();
    client.watch_machine();
    let create = json!({"op_id": op(), "agent": "fake", "cwd": env.work(), "sandbox_level": "off",
                        "prompt": {"content": text("fix the flaky test")}});
    let created = client.call("session.create", create.clone());
    assert_eq!(created["prompt"]["outcome"], "queued");
    assert_eq!(created["session"]["queue"]["len"], 1);
    assert_eq!(created["session"]["agent_live"], false);
    // A retry (e.g. after a lost response) is the same command.
    assert_eq!(client.call("session.create", create), created);
    let id = created["session"]["id"].as_str().unwrap().to_owned();
    let s = client.wait_turn(&id, 0);
    assert!(s["preview"].as_str().unwrap().contains("Started on it"));
    assert_eq!(s["agent_live"], true);
    assert_eq!(env.prompts_sent(&id), ["fix the flaky test"]);

    // Detail only: the result and later events carry no timeline ops.
    let result = client.call(
        "session.subscribe",
        json!({"id": id, "since_seq": 0, "timeline": false}),
    );
    let sub = result["subscription_id"].clone();
    assert_eq!(result["detail"]["queue"], json!([]));
    let mark = client.turn_mark(&id);
    client.prompt(&id, "again");
    client.wait_turn(&id, mark);
    // A full subscription after it sees the new turn; the detail-only one got no ops.
    let (_, ops) = client.session_ops(&id, mark);
    assert!(!ops.is_empty());
    assert!(
        client
            .notifications()
            .iter()
            .filter(|n| n["params"]["subscription_id"] == sub)
            .all(|n| n["params"]["ops"].as_array().is_none_or(Vec::is_empty)),
    );
}

#[test]
fn op_id_replay_applies_once() {
    let env = Env::with(json!({}));
    let client = env.client();
    client.watch_machine();
    let create =
        json!({"op_id": "create-1", "agent": "fake", "cwd": env.work(), "sandbox_level": "off"});
    let first = client.call("session.create", create.clone());
    let again = client.call("session.create", create);
    assert_eq!(first["session"]["id"], again["session"]["id"]);
    let id = first["session"]["id"].as_str().unwrap().to_owned();
    client.wait_idle(&id);

    let prompt = json!({"op_id": "prompt-1", "id": id, "content": text("only once")});
    let first = client.call("session.prompt", prompt.clone());
    // From another connection too.
    let other = env.client();
    assert_eq!(other.call("session.prompt", prompt), first);
    client.wait_session(&id, "the turn end", |s| {
        s["state"] == "idle" && s["last_outcome"] == "completed"
    });
    assert_eq!(env.prompts_sent(&id), ["only once"]);
    client.call_err(
        "session.cancel",
        json!({"op_id": "prompt-1", "id": id}),
        "op_id_conflict",
    );
    let snapshot = env.client().watch_machine();
    assert_eq!(
        snapshot["snapshot"]["sessions"].as_array().unwrap().len(),
        1
    );
}

fn option_value(summary: &Value, id: &str) -> Value {
    summary["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == id)
        .map(|o| o["value"].clone())
        .unwrap_or(Value::Null)
}

#[test]
fn set_option_model_and_thinking_including_during_a_turn() {
    let env = Env::with(json!({"replies": [gated("Working", "busy", " done.")]}));
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    // Choices arrive with the state refresh after start (only the value before that).
    client.wait_session(&id, "options", |s| {
        s["state"] == "idle"
            && s["options"].as_array().unwrap().iter().any(|o| {
                o["id"] == "model" && o["choices"].as_array().is_some_and(|c| c.len() == 2)
            })
    });

    let set = |option: &str, value: &str| {
        client.call(
            "session.set_option",
            json!({"op_id": op(), "id": id, "option_id": option, "value": value}),
        )
    };
    set("model", "mock/mock-model-b");
    client.wait_session(&id, "the new model", |s| {
        option_value(s, "model") == "mock/mock-model-b"
    });

    client.prompt(&id, "go");
    env.wait_gate("busy");
    set("thinking", "high");
    client.wait_session(&id, "the new thinking level", |s| {
        option_value(s, "thinking") == "high"
    });
    env.release("busy");
    client.wait_turn(&id, 0);

    let commands: Vec<String> = env
        .lines_in(&id)
        .iter()
        .filter_map(|l| l["type"].as_str().map(str::to_owned))
        .filter(|t| t.starts_with("set_"))
        .collect();
    assert_eq!(commands, ["set_model", "set_thinking_level"]);
    let err = client.call_err(
        "session.set_option",
        json!({"op_id": op(), "id": id, "option_id": "nope", "value": "x"}),
        "not_found",
    );
    assert_eq!(err["data"]["resource"], "option");
    client.call_err(
        "session.set_option",
        json!({"op_id": op(), "id": id, "option_id": "model", "value": "nope/none"}),
        "invalid_params",
    );
}

#[test]
fn context_usage_is_on_the_summary() {
    let env = Env::with(json!({"replies": [
        {"content": [{"type": "text", "text": "counted"}], "usage": {"input": 500, "output": 40}}
    ]}));
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    client.prompt(&id, "count");
    let summary = client.wait_session(&id, "context usage", |s| {
        s["state"] == "idle" && s["context"]["used"].as_u64().is_some_and(|u| u >= 540)
    });
    assert_eq!(summary["context"]["size"], 128_000);
}

#[test]
fn start_fresh_with_history() {
    let env = Env::with(texts(&["Remember 42.", "Fresh start."]));
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    client.prompt(&id, "remember");
    client.wait_session(&id, "the first turn", |s| s["last_outcome"] == "completed");

    let result = client.call(
        "session.start_fresh",
        json!({"op_id": op(), "id": id, "include_history": true}),
    );
    let path = result["history_path"].as_str().unwrap().to_owned();
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("Remember 42.")
    );
    client.wait_idle(&id);
    client.prompt(&id, "again");
    client.wait_session(&id, "the second turn", |s| {
        s["state"] == "idle" && s["preview"].as_str().unwrap().contains("Fresh start")
    });
    // The first prompt of the new agent session points at the export.
    let sent = env.prompts_sent(&id);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(
        sent[1].contains(&path) && sent[1].contains("again"),
        "{sent:?}"
    );
    let (sub, _) = client.session_ops(&id, 0);
    assert_eq!(sub["detail"]["agent_sessions"].as_array().unwrap().len(), 2);
}

#[test]
fn close_reopen_delete() {
    let env = Env::with(json!({}));
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    let (supervisor, _) = env.supervisor_pids(&id);

    client.call("session.close", json!({"op_id": op(), "id": id}));
    client.wait_session(&id, "closed", |s| s["state"] == "closed");
    wait_until("the supervisor to exit", || !alive(supervisor));
    client.call_err(
        "session.prompt",
        json!({"op_id": op(), "id": id, "content": text("x")}),
        "invalid_state",
    );

    client.call("session.reopen", json!({"op_id": op(), "id": id}));
    client.wait_session(&id, "dormant", |s| s["state"] == "dormant");

    client.call("session.delete", json!({"op_id": op(), "id": id}));
    client.wait_notification("session_removed", |n| {
        n["params"]["changes"].as_array().is_some_and(|c| {
            c.iter()
                .any(|c| c["kind"] == "session_removed" && c["id"] == id)
        })
    });
    let dir = env.state().join("sessions").join(&id);
    wait_until("the session files to go", || !dir.exists());
    client.call_err(
        "session.reopen",
        json!({"op_id": op(), "id": id}),
        "unknown_session",
    );
}

/// Whether `pid` is a live (non-zombie) process.
pub fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| !s.rsplit(") ").next().unwrap_or("").starts_with('Z'))
        .unwrap_or(false)
}
