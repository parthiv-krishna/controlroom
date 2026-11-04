//! The agent socket end to end: the fake agent plays `@controlroom/pi` (its tool calls call
//! the session's agent socket with the launch's token), and the daemon turns that into todos,
//! a non-blocking question, its injected answer and auto-continue runs.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::harness::*;

/// A tool call that calls the agent API (the fake agent's stand-in for the extension).
fn agent_tool(id: &str, name: &str, method: &str, params: Value) -> Value {
    json!({"content": [{"type": "tool_call", "id": id, "name": name, "args": params.clone(),
        "agent": {"method": method, "params": params}}]})
}

fn say(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

fn agent_socket(env: &Env, session: &str) -> PathBuf {
    env.state().join(format!("run/{session}.agent.sock"))
}

fn agent_token(env: &Env, session: &str) -> String {
    std::fs::read_to_string(env.state().join(format!("sessions/{session}/agent.token")))
        .unwrap()
        .trim()
        .to_owned()
}

fn agent_hello(token: &str) -> Value {
    json!({"protocol_version": {"major": 1, "minor": 0}, "token": token, "client_name": "e2e"})
}

/// Run triggers of the session's timeline, as `kind` or `inject:<origin>`.
fn triggers(client: &Client, session: &str) -> Vec<String> {
    let (result, ops) = client.session_ops(session, 0);
    client.call(
        "unsubscribe",
        json!({"subscription_id": result["subscription_id"]}),
    );
    ops.iter()
        .filter(|op| op["item"]["kind"] == "run")
        .map(|op| {
            let trigger = &op["item"]["trigger"];
            match trigger["kind"].as_str().unwrap() {
                "inject" => format!("inject:{}", trigger["origin"].as_str().unwrap()),
                kind => kind.to_owned(),
            }
        })
        .collect()
}

#[test]
fn the_extension_drives_todos_a_question_and_auto_continue() {
    let scenario = json!({"replies": [
        agent_tool("call_plan", "todo", "todo.add", json!({"texts": ["migrate", "document"]})),
        say("Planned two items."),
        agent_tool("call_ask", "ask_user", "question.ask",
            json!({"questions": [{"text": "Which database?", "options": [{"label": "Postgres"}]}]})),
        say("Asked; nothing else to do until the answer."),
        say("Thanks, using Postgres."),
        say("Still working."),
        say("Still working."),
    ]});
    let env = Env::with(scenario);
    let client = Client::connect(&env.socket());
    let hello = client.hello_debug();
    assert!(
        hello["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("todos"))
    );
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);

    // The agent plans two items, ends its turn, and auto-continue starts the next run, in
    // which the agent asks a question and stops: the question doesn't block, and its answer,
    // not auto-continue, will wake the agent.
    client.prompt(&id, "plan the migration");
    let s = client.wait_session(&id, "the open question", |s| {
        s["state"] == "idle"
            && s["requests"]
                .as_array()
                .is_some_and(|r| r.iter().any(|r| r["kind"] == "question"))
    });
    assert_eq!(s["attention"], "question");
    assert_eq!(s["todo"]["total"], 2);
    assert_eq!(s["todo"]["auto_continue"], "on");
    let question = s["requests"][0].clone();
    assert_eq!(question["blocking"], false);
    assert_eq!(question["title"], "Which database?");
    // The tool result told the model the reminder.
    let reminder = env
        .records(&id)
        .iter()
        .filter_map(|r| match &r.body {
            controlroom_transcript::Body::Out(line) => {
                serde_json::from_str::<Value>(&line.raw).ok()
            }
            _ => None,
        })
        .filter(|v| v["type"] == "tool_execution_end" && v["toolCallId"] == "call_ask")
        .map(|v| v["result"].to_string())
        .next()
        .unwrap();
    assert!(reminder.contains("the answer will wake you"), "{reminder}");

    // The user edits the list (attributed to the device the controller names).
    let todos = client.call(
        "todo.add",
        json!({"op_id": op(), "id": id, "texts": ["release"],
               "by": {"device_id": "laptop-1", "label": "Laptop"}}),
    );
    assert_eq!(todos["todos"][0]["updated_by"]["kind"], "user");
    assert_eq!(todos["todos"][0]["updated_by"]["device"]["label"], "Laptop");

    // The answer wakes the agent; two more continue runs make no progress: stalled.
    let mark = client.turn_mark(&id);
    client.call(
        "request.respond",
        json!({"op_id": op(), "id": id, "request_id": question["id"],
               "response": {"kind": "answers", "answers": [
                   {"question_id": "q1", "selected": ["Postgres"]}]}}),
    );
    let s = client.wait_session(&id, "stalled", |s| {
        s["todo"]["auto_continue"] == "stalled" && s["state"] == "idle"
    });
    assert!(s["last_turn_end_seq"].as_u64() > Some(mark));
    assert_eq!(s["attention"], "problem");
    assert!(s["requests"].as_array().unwrap().is_empty());
    let prompts = env.prompts_sent(&id);
    assert!(
        prompts
            .iter()
            .any(|p| p.contains("answered your question") && p.contains("→ Postgres")),
        "{prompts:#?}"
    );
    // The user's todo edit rode along into the next run.
    assert!(
        prompts.iter().any(|p| p.contains("added \"release\"")),
        "{prompts:#?}"
    );
    assert_eq!(
        triggers(&client, &id),
        [
            "user",
            "auto_continue",
            "inject:answer",
            "auto_continue",
            "auto_continue"
        ]
    );
}

#[test]
fn the_agent_socket_takes_only_its_session_token_and_survives_a_restart() {
    let mut env = Env::with(json!({}));
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    let socket = agent_socket(&env, &id);
    let token = agent_token(&env, &id);

    let agent = Client::connect(&socket);
    agent.call_err("todo.list", json!({}), "hello_required");
    agent.call_err("hello", agent_hello(&"0".repeat(64)), "unauthenticated");
    let hello = agent.call("hello", agent_hello(&token));
    assert_eq!(hello["session_id"], json!(id));
    // A state notification follows the hello.
    let state = agent.wait_notification("the agent state", |n| n["method"] == "state");
    assert_eq!(state["params"]["auto_continue"]["status"], "on");
    agent.call(
        "notify",
        json!({"call_id": "n1", "text": "Deployed to staging", "level": "warning"}),
    );
    let (_, ops) = client.session_ops(&id, 0);
    assert!(ops.iter().any(|op| op["item"]["kind"] == "notice"
        && op["item"]["text"] == "Deployed to staging"));

    // The daemon restarts; the agent outlives it and reconnects to the same socket.
    env.kill_daemon();
    env.start();
    // The daemon binds agent sockets once its runtime is open; like the extension, retry.
    wait_until("the agent socket", || {
        std::os::unix::net::UnixStream::connect(&socket).is_ok()
    });
    let agent = Client::connect(&socket);
    agent.call("hello", agent_hello(&token));
    let todos = agent.call(
        "todo.add",
        json!({"call_id": "t1", "texts": ["after restart"]}),
    );
    assert_eq!(todos["todos"][0]["updated_by"]["kind"], "agent");
    // The same call id applies once.
    let again = agent.call(
        "todo.add",
        json!({"call_id": "t1", "texts": ["after restart"]}),
    );
    assert_eq!(again["todos"].as_array().unwrap().len(), 1);

    // Deleting the session removes its socket.
    let client = env.client();
    client.call("session.delete", json!({"op_id": op(), "id": id}));
    wait_until("the agent socket to go", || !socket.exists());
}
