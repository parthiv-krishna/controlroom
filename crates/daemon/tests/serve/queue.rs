//! The queue: auto-advance, pause on failure, send-now (pi steers), cancel.

use serde_json::{Value, json};

use crate::harness::*;

fn queued(result: &Value) -> String {
    assert_eq!(result["outcome"], "queued", "{result}");
    result["queue_id"].as_str().unwrap().to_owned()
}

#[test]
fn queue_auto_advances_and_pauses_on_failure() {
    let env = Env::with(json!({"replies": [
        gated("One", "first", "."),
        {"content": [{"type": "text", "text": "Two."}]},
        {"error": "invalid request: bad input"},
        {"content": [{"type": "text", "text": "Four."}]},
    ]}));
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    assert_eq!(client.prompt(&id, "first")["outcome"], "sent");
    env.wait_gate("first");
    for prompt in ["second", "third", "fourth"] {
        queued(&client.prompt(&id, prompt));
    }
    env.release("first");
    let paused = client.wait_session(&id, "the queue to pause", |s| s["queue"]["paused"] == true);
    assert_eq!(paused["queue"]["len"], 1);
    assert_eq!(queue_texts(&client, &id), ["fourth"]);
    assert_eq!(env.prompts_sent(&id), ["first", "second", "third"]);

    // No summary showed an idle session with an empty queue while items were still waiting.
    for n in client.notifications() {
        for change in n["params"]["changes"].as_array().into_iter().flatten() {
            let s = &change["session"];
            if s["id"] == id && s["state"] == "idle" {
                assert!(
                    s["queue"]["len"] == 0 || s["queue"]["paused"] == true,
                    "idle while the queue advanced: {s}"
                );
            }
        }
    }

    client.call("queue.resume", json!({"op_id": op(), "id": id}));
    client.wait_idle(&id);
    assert_eq!(env.prompts_sent(&id), ["first", "second", "third", "fourth"]);
}

/// Prompts `first` (held at gate `hold` inside `reply`) and queues `second`; returns the
/// session and the queue id.
fn busy_with_queue(env: &Env, client: &Client) -> (String, String) {
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    client.prompt(&id, "first");
    env.wait_gate("hold");
    let queue_id = queued(&client.prompt(&id, "second"));
    (id, queue_id)
}

fn send_now(client: &Client, id: &str, queue_id: &str) -> Value {
    client.call(
        "queue.send_now",
        json!({"op_id": op(), "id": id, "queue_id": queue_id}),
    )
}

#[test]
fn send_now_during_text_steers() {
    let env = Env::with(json!({"replies": [gated("Writing", "hold", " more.")]}));
    let client = env.client();
    client.watch_machine();
    let (id, queue_id) = busy_with_queue(&env, &client);
    assert_eq!(send_now(&client, &id, &queue_id)["outcome"], "steered");
    env.release("hold");
    client.wait_idle(&id);
    let steers: Vec<Value> = env
        .lines_in(&id)
        .into_iter()
        .filter(|l| l["type"] == "steer")
        .collect();
    assert_eq!(steers.len(), 1);
    assert_eq!(steers[0]["message"], "second");
    assert_eq!(env.prompts_sent(&id), ["first", "second"]);
}

#[test]
fn send_now_during_a_tool_steers_after_the_tool() {
    let env = Env::with(json!({"replies": [
        {"content": [{"type": "tool_call", "id": "call_1", "name": "bash", "args": {"command": "make"},
                      "result": "built", "progress": [{"update": "compiling"}, {"gate": "hold"}]}]},
        {"content": [{"type": "text", "text": "Built."}]},
    ]}));
    let client = env.client();
    client.watch_machine();
    let (id, queue_id) = busy_with_queue(&env, &client);
    assert_eq!(send_now(&client, &id, &queue_id)["outcome"], "steered");
    assert_eq!(client.summary(&id)["queue"]["len"], 1, "still waiting for the agent");
    env.release("hold");
    client.wait_idle(&id);
    // The tool completed: steering didn't interrupt it.
    let (_, ops) = client.session_ops(&id, 0);
    assert!(ops.iter().any(|op| {
        op["item"]["kind"] == "tool_call" && op["item"]["status"] == "completed"
    }));
    assert_eq!(env.prompts_sent(&id), ["first", "second"]);
}

#[test]
fn send_now_during_a_dialog_steers_and_the_dialog_stays_open() {
    let env = Env::with(json!({"replies": [
        {"content": [{"type": "gate", "name": "hold"},
                     {"type": "tool_call", "id": "call_1", "name": "bash", "args": {"command": "rm x"},
                      "result": "removed", "permission": {"title": "Run rm x?"}}]},
        {"content": [{"type": "text", "text": "Done."}]},
    ]}));
    let client = env.client();
    client.watch_machine();
    let (id, queue_id) = busy_with_queue(&env, &client);
    env.release("hold");
    let s = client.wait_session(&id, "the dialog", |s| {
        !s["requests"].as_array().unwrap().is_empty()
    });
    let request = s["requests"][0]["id"].clone();
    assert_eq!(send_now(&client, &id, &queue_id)["outcome"], "steered");
    client.call(
        "request.respond",
        json!({"op_id": op(), "id": id, "request_id": request,
               "response": {"kind": "choice", "option_id": "0"}}),
    );
    client.wait_idle(&id);
    assert_eq!(env.prompts_sent(&id), ["first", "second"]);
}

#[test]
fn cancel_pauses_the_queue() {
    let env = Env::with(json!({"replies": [gated("Long", "hold", " answer.")]}));
    let client = env.client();
    client.watch_machine();
    let (id, _) = busy_with_queue(&env, &client);
    client.call("session.cancel", json!({"op_id": op(), "id": id}));
    let s = client.wait_session(&id, "the cancelled run", |s| {
        s["state"] == "idle" && s["last_outcome"] == "cancelled"
    });
    assert_eq!(s["queue"]["paused"], true);
    assert_eq!(s["queue"]["len"], 1);
    assert_eq!(env.prompts_sent(&id), ["first"]);
    client.call("queue.resume", json!({"op_id": op(), "id": id}));
    client.wait_idle(&id);
    assert_eq!(env.prompts_sent(&id), ["first", "second"]);
}
