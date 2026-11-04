//! The queue: auto-advance, pause on failure, send-now (pi steers, or starts the next run when
//! the steer reaches it after its run), cancel.

use controlroom_transcript::Body;
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
    client.watch_sessions();
    let id = client.create(&env.workspaces());
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

    let mark = client.turn_mark(&id);
    client.call("queue.resume", json!({"op_id": op(), "id": id}));
    client.wait_turn(&id, mark);
    assert_eq!(
        env.prompts_sent(&id),
        ["first", "second", "third", "fourth"]
    );
}

/// Prompts `first` (held at gate `hold` inside `reply`) and queues `second`; returns the
/// session and the queue id.
///
/// The agent reaching the gate doesn't mean the daemon has folded its output yet: tests that
/// depend on what the daemon saw of the run wait for it with [`wait_preview`].
fn busy_with_queue(env: &Env, client: &Client) -> (String, String) {
    let id = client.create(&env.workspaces());
    client.wait_idle(&id);
    client.prompt(&id, "first");
    env.wait_gate("hold");
    let queue_id = queued(&client.prompt(&id, "second"));
    (id, queue_id)
}

/// Waits until the daemon shows `preview` for the working session: it has folded the run's
/// start and its output up to there (a send-now steers only into a run the daemon knows is
/// active; before that it would wait for the run to start).
fn wait_preview(client: &Client, id: &str, preview: &str) {
    client.wait_session(id, &format!("preview {preview:?}"), |s| {
        s["state"] == "working" && s["preview"] == preview
    });
}

/// The lines that steered a send-now in: `prompt` with `streamingBehavior: "steer"`, which
/// starts the next run if the agent reads it after its run ended.
fn steers(env: &Env, id: &str) -> Vec<Value> {
    env.lines_in(id)
        .into_iter()
        .filter(|l| l["type"] == "prompt" && l["streamingBehavior"] == "steer")
        .collect()
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
    client.watch_sessions();
    let (id, queue_id) = busy_with_queue(&env, &client);
    wait_preview(&client, &id, "Writing");
    assert_eq!(send_now(&client, &id, &queue_id)["outcome"], "steered");
    env.release("hold");
    client.wait_turn(&id, 0);
    let steers = steers(&env, &id);
    assert_eq!(steers.len(), 1);
    assert_eq!(steers[0]["message"], "second");
    assert_eq!(env.prompts_sent(&id), ["first", "second"]);
}

/// A send-now steered into a run that ends before the agent reads the line starts the next run
/// without another message from the user (pi would hold a plain `steer` until then).
#[test]
fn send_now_racing_the_end_of_the_run_starts_the_next_run() {
    let env = Env::with(json!({"replies": [
        {"content": [
            {"type": "text", "text": "Writing"},
            {"type": "deafen", "until": "listen"},
            {"type": "gate", "name": "hold"},
            {"type": "text", "text": " more."},
        ]},
        {"content": [{"type": "text", "text": "Changed course."}]},
    ]}));
    let client = env.client();
    client.watch_sessions();
    let (id, queue_id) = busy_with_queue(&env, &client);
    wait_preview(&client, &id, "Writing");
    assert_eq!(send_now(&client, &id, &queue_id)["outcome"], "steered");
    // Logged by the supervisor, but the agent doesn't read it before its run has ended.
    wait_until("the steer line", || !steers(&env, &id).is_empty());
    env.release("hold");
    wait_until("the run's end", || {
        env.records(&id)
            .iter()
            .any(|r| matches!(&r.body, Body::Out(l) if l.raw.contains(r#""type":"agent_settled""#)))
    });
    env.release("listen");
    client.wait_session(&id, "the steered run", |s| {
        s["state"] == "idle" && s["queue"]["len"] == 0 && s["preview"] == "Changed course."
    });
    assert_eq!(env.prompts_sent(&id), ["first", "second"]);
    let (_, ops) = client.session_ops(&id, 0);
    let triggers: Vec<&Value> = ops
        .iter()
        .filter(|op| op["item"]["kind"] == "run")
        .map(|op| &op["item"]["trigger"]["kind"])
        .collect();
    assert_eq!(
        triggers.last().copied(),
        Some(&json!("steer")),
        "{triggers:?}"
    );
}

#[test]
fn send_now_during_a_tool_steers_after_the_tool() {
    let env = Env::with(json!({"replies": [
        {"content": [{"type": "tool_call", "id": "call_1", "name": "bash", "args": {"command": "make"},
                      "result": "built", "progress": [{"update": "compiling"}, {"gate": "hold"}]}]},
        {"content": [{"type": "text", "text": "Built."}]},
    ]}));
    let client = env.client();
    client.watch_sessions();
    let (id, queue_id) = busy_with_queue(&env, &client);
    // The running tool's title.
    wait_preview(&client, &id, "make");
    assert_eq!(send_now(&client, &id, &queue_id)["outcome"], "steered");
    wait_until("the steer line", || !steers(&env, &id).is_empty());
    env.release("hold");
    client.wait_turn(&id, 0);
    // The steer went in while the tool ran, and the tool still completed.
    let records = env.records(&id);
    let steer_seq = records
        .iter()
        .find(
            |r| matches!(&r.body, Body::In(l) if l.raw.contains(r#""streamingBehavior":"steer""#)),
        )
        .map(|r| r.seq)
        .expect("a steer line");
    let tool_end_seq = records
        .iter()
        .find(|r| matches!(&r.body, Body::Out(l) if l.raw.contains("\"tool_execution_end\"")))
        .map(|r| r.seq)
        .expect("the tool end");
    assert!(steer_seq < tool_end_seq);
    let (_, ops) = client.session_ops(&id, 0);
    assert!(
        ops.iter()
            .any(|op| { op["item"]["kind"] == "tool_call" && op["item"]["status"] == "completed" })
    );
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
    client.watch_sessions();
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
    client.wait_turn(&id, 0);
    assert_eq!(env.prompts_sent(&id), ["first", "second"]);
}

#[test]
fn cancel_pauses_the_queue() {
    let env = Env::with(json!({"replies": [gated("Long", "hold", " answer.")]}));
    let client = env.client();
    client.watch_sessions();
    let (id, _) = busy_with_queue(&env, &client);
    client.call("session.cancel", json!({"op_id": op(), "id": id}));
    let s = client.wait_session(&id, "the cancelled run", |s| {
        s["state"] == "idle" && s["last_outcome"] == "cancelled"
    });
    assert_eq!(s["queue"]["paused"], true);
    assert_eq!(s["queue"]["len"], 1);
    assert_eq!(env.prompts_sent(&id), ["first"]);
    let mark = client.turn_mark(&id);
    client.call("queue.resume", json!({"op_id": op(), "id": id}));
    client.wait_turn(&id, mark);
    assert_eq!(env.prompts_sent(&id), ["first", "second"]);
}
