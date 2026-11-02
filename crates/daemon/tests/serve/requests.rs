//! Requests: extension dialogs answered over the socket, and two clients racing a decision.

use serde_json::{Value, json};

use crate::harness::*;

/// Every turn runs a tool behind a permission dialog, then says "done".
fn permission_scenario() -> Value {
    let turn = json!({"content": [{"type": "tool_call", "id": "call_1", "name": "bash",
        "args": {"command": "rm -rf build"}, "result": "removed",
        "permission": {"title": "Run rm -rf build?"}}]});
    let done = json!({"content": [{"type": "text", "text": "done"}]});
    json!({"replies": [turn.clone(), done.clone(), turn.clone(), done, turn]})
}

fn pending_request(client: &Client, id: &str) -> Value {
    let s = client.wait_session(id, "a pending request", |s| {
        s["requests"]
            .as_array()
            .is_some_and(|r| r.iter().any(|r| r["state"] == "pending"))
    });
    assert_eq!(s["state"], "awaiting_input");
    assert_eq!(s["attention"], "approval");
    s["requests"][0].clone()
}

fn respond(client: &Client, id: &str, request: &Value, option: &str) -> Result<Value, Value> {
    client.try_call(
        "request.respond",
        json!({"op_id": op(), "id": id, "request_id": request["id"],
               "response": {"kind": "choice", "option_id": option}}),
    )
}

/// The `extension_ui_response` lines written to the agent.
fn answers(env: &Env, id: &str) -> Vec<Value> {
    env.lines_in(id)
        .into_iter()
        .filter(|l| l["type"] == "extension_ui_response")
        .collect()
}

#[test]
fn dialog_approve_and_deny() {
    let env = Env::with(permission_scenario());
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);

    client.prompt(&id, "clean up");
    let request = pending_request(&client, &id);
    assert_eq!(request["title"], "Run rm -rf build?");
    let result = respond(&client, &id, &request, "0").unwrap();
    assert_eq!(result["request"]["state"], "answered");
    let first = client.wait_session(&id, "the first turn", |s| {
        s["state"] == "idle" && s["last_outcome"] == "completed"
    });
    assert!(first["requests"].as_array().unwrap().is_empty());

    client.prompt(&id, "again");
    let request = pending_request(&client, &id);
    respond(&client, &id, &request, "1").unwrap();
    client.wait_session(&id, "the second turn", |s| {
        s["state"] == "idle" && s["last_turn_end_seq"] != first["last_turn_end_seq"]
    });
    let (_, ops) = client.session_ops(&id, 0);
    let statuses: Vec<&str> = ops
        .iter()
        .filter(|op| op["item"]["kind"] == "tool_call")
        .filter_map(|op| op["item"]["status"].as_str())
        .collect();
    assert_eq!(statuses, ["completed", "cancelled"], "allowed, then blocked");
    let values: Vec<String> = answers(&env, &id)
        .iter()
        .filter_map(|a| a["value"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(values, ["Allow", "Block"]);

    // An option the request doesn't offer is refused.
    client.prompt(&id, "third");
    let request = pending_request(&client, &id);
    let err = respond(&client, &id, &request, "7").unwrap_err();
    assert_eq!(err["data"]["kind"], "invalid_params");
}

#[test]
fn two_clients_race_a_decision() {
    let env = Env::with(permission_scenario());
    let setup = env.client();
    setup.watch_machine();
    let id = setup.create("fake", &env.work());
    setup.wait_idle(&id);
    setup.prompt(&id, "go");
    let request = pending_request(&setup, &id);

    let racers: Vec<_> = ["0", "1"]
        .into_iter()
        .map(|option| {
            let client = env.client();
            let (id, request) = (id.clone(), request.clone());
            std::thread::spawn(move || respond(&client, &id, &request, option))
        })
        .collect();
    let results: Vec<Result<Value, Value>> =
        racers.into_iter().map(|t| t.join().unwrap()).collect();
    let winners: Vec<&Value> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    let losers: Vec<&Value> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert_eq!((winners.len(), losers.len()), (1, 1), "{results:?}");
    assert_eq!(losers[0]["data"]["kind"], "already_resolved");
    assert_eq!(
        losers[0]["data"]["request"]["answer"],
        winners[0]["request"]["answer"]
    );

    setup.wait_session(&id, "the turn end", |s| {
        s["state"] == "idle" && s["last_turn_end_seq"].as_u64() > Some(0)
    });
    assert_eq!(answers(&env, &id).len(), 1, "the agent got exactly one answer");
    // A late answer learns the winner too.
    let late = respond(&setup, &id, &request, "0").unwrap_err();
    assert_eq!(late["data"]["kind"], "already_resolved");
}

#[test]
fn an_answer_is_sent_once_across_a_daemon_restart() {
    let mut env = Env::with(permission_scenario());
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    client.prompt(&id, "go");
    let request = pending_request(&client, &id);
    respond(&client, &id, &request, "0").unwrap();
    // Killed right after the decision commits: the answer may or may not have been written.
    env.kill_daemon();
    env.start();
    let client = env.client();
    client.watch_machine();
    client.wait_session(&id, "the turn end", |s| {
        s["state"] == "idle" && s["last_outcome"] == "completed"
    });
    assert_eq!(answers(&env, &id).len(), 1, "no duplicate answer");
    assert_eq!(env.prompts_sent(&id), ["go"], "no duplicate prompt");
}
