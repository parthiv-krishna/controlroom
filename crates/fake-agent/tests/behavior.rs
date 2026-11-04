//! Behavior of fake-agent features that tests elsewhere rely on: gates,
//! crashes, persistence and resume.

mod support;

use serde_json::{Value, json};
use support::{Sandbox, WAIT};

fn prompt(text: &str) -> Value {
    json!({"type": "prompt", "message": text})
}

#[tokio::test]
async fn gate_holds_the_run_until_the_test_releases_it() {
    let sandbox = Sandbox::new();
    let scenario = r#"{"replies": [{"content": [
        {"type": "text", "text": "before"}, {"type": "gate", "name": "g1"},
        {"type": "text", "text": " after"}]}]}"#;
    let mut fake = sandbox.spawn(scenario, &[]);
    fake.call("1", prompt("go")).await;
    let gates = sandbox.gates();
    gates.wait_reached("g1", WAIT).await.unwrap();
    // While held, the agent still answers commands and reports streaming.
    let state = fake.call("2", json!({"type": "get_state"})).await;
    assert_eq!(state["data"]["isStreaming"], true);
    assert!(fake.records().iter().all(|r| r["type"] != "agent_settled"));
    gates.release("g1").unwrap();
    let end = fake.next_event("message_end").await;
    assert_eq!(end["message"]["content"][0]["text"], "before after");
    fake.next_event("agent_settled").await;
    fake.close_stdin();
    assert_eq!(fake.wait_exit().await, (Some(0), None));
}

#[tokio::test]
async fn deafen_leaves_lines_unread_until_the_test_releases_it() {
    let sandbox = Sandbox::new();
    let scenario = r#"{"replies": [
        {"content": [{"type": "text", "text": "before"}, {"type": "deafen", "until": "listen"},
                     {"type": "gate", "name": "g1"}, {"type": "text", "text": " after"}]},
        {"content": [{"type": "text", "text": "late one"}]}]}"#;
    let mut fake = sandbox.spawn(scenario, &[]);
    fake.call("1", prompt("go")).await;
    let gates = sandbox.gates();
    gates.wait_reached("g1", WAIT).await.unwrap();
    fake.send(
        json!({"id": "2", "type": "prompt", "message": "steer", "streamingBehavior": "steer"}),
    )
    .await;
    gates.release("g1").unwrap();
    fake.next_event("agent_settled").await;
    assert!(
        fake.records().iter().all(|r| r["id"] != "2"),
        "not read yet"
    );
    // Read while idle, the steering prompt starts a run (like pi).
    gates.release("listen").unwrap();
    let response = fake.next("the response", |r| r["id"] == "2").await;
    assert_eq!(response["data"]["disposition"], "started");
    let end = fake.next_event("message_end").await;
    assert_eq!(end["message"]["role"], "user");
    let end = fake.next_event("agent_settled").await;
    assert_eq!(end["aborted"], false);
    fake.close_stdin();
    assert_eq!(fake.wait_exit().await, (Some(0), None));
}

#[tokio::test]
async fn crash_step_exits_mid_run_with_its_code() {
    let sandbox = Sandbox::new();
    let scenario = r#"{"replies": [{"content": [
        {"type": "text", "text": "partial"}, {"type": "crash", "code": 7}]}]}"#;
    let mut fake = sandbox.spawn(scenario, &[]);
    fake.send(prompt("go")).await;
    assert_eq!(fake.wait_exit().await, (Some(7), None));
    assert!(fake.records().iter().all(|r| r["type"] != "agent_end"));
}

#[tokio::test]
async fn session_resumes_from_its_file() {
    let sandbox = Sandbox::new();
    let mut first = sandbox.spawn("{}", &["--session-id".into(), "s-1".into()]);
    first.call("1", prompt("remember apple")).await;
    first.next_event("agent_settled").await;
    let state = first.call("2", json!({"type": "get_state"})).await;
    assert_eq!(state["data"]["sessionId"], "s-1");
    assert_eq!(state["data"]["messageCount"], 3);
    let file = state["data"]["sessionFile"].as_str().unwrap().to_owned();
    first.close_stdin();
    first.wait_exit().await;

    // By id: the existing session is reopened.
    let mut by_id = sandbox.spawn("{}", &["--session-id".into(), "s-1".into()]);
    let state = by_id.call("1", json!({"type": "get_state"})).await;
    assert_eq!(state["data"]["messageCount"], 3);
    by_id.close_stdin();
    by_id.wait_exit().await;

    // By file: history is restored and no second system message is sent.
    let mut second = sandbox.spawn("{}", &["--session".into(), file]);
    let messages = second.call("1", json!({"type": "get_messages"})).await;
    assert_eq!(
        messages["data"]["messages"][1]["content"][0]["text"],
        "remember apple"
    );
    second.call("2", prompt("what was it?")).await;
    second.next_event("agent_settled").await;
    let systems = second
        .records()
        .iter()
        .filter(|r| r["type"] == "message_start" && r["message"]["role"] == "system")
        .count();
    assert_eq!(systems, 0);
    let state = second.call("3", json!({"type": "get_state"})).await;
    assert_eq!(state["data"]["messageCount"], 5);
    second.close_stdin();
    second.wait_exit().await;
}

#[tokio::test]
async fn no_session_writes_nothing() {
    let sandbox = Sandbox::new();
    let mut fake = sandbox.spawn("{}", &["--no-session".into()]);
    fake.call("1", prompt("hi")).await;
    fake.next_event("agent_settled").await;
    let state = fake.call("2", json!({"type": "get_state"})).await;
    assert!(state["data"].get("sessionFile").is_none());
    fake.close_stdin();
    fake.wait_exit().await;
    assert!(!sandbox.path("agent/sessions").exists());
}

#[tokio::test]
async fn new_session_records_its_parent() {
    let sandbox = Sandbox::new();
    let mut fake = sandbox.spawn("{}", &[]);
    fake.call("1", prompt("hi")).await;
    fake.next_event("agent_settled").await;
    let old = fake.call("2", json!({"type": "get_state"})).await;
    let parent = old["data"]["sessionFile"].as_str().unwrap().to_owned();
    fake.call("3", json!({"type": "new_session", "parentSession": parent}))
        .await;
    fake.call("4", prompt("again")).await;
    fake.next_event("agent_settled").await;
    let new = fake.call("5", json!({"type": "get_state"})).await;
    assert_ne!(new["data"]["sessionId"], old["data"]["sessionId"]);
    let file = new["data"]["sessionFile"].as_str().unwrap();
    let header: Value = serde_json::from_str(
        std::fs::read_to_string(file)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(header["parentSession"], parent);
    fake.close_stdin();
    fake.wait_exit().await;
}

#[tokio::test]
async fn retry_cancelled_by_abort_settles_aborted() {
    let sandbox = Sandbox::new();
    let scenario = r#"{"retry_base_delay_ms": 60000, "replies": [{"error": "503 overloaded"}]}"#;
    let mut fake = sandbox.spawn(scenario, &[]);
    fake.call("1", prompt("go")).await;
    fake.next_event("auto_retry_start").await;
    // pi's abort answers only once the run settled, so the events precede it.
    fake.call("2", json!({"type": "abort"})).await;
    let records = fake.records();
    let kinds: Vec<&str> = records.iter().filter_map(|r| r["type"].as_str()).collect();
    let tail = &kinds[kinds.len() - 3..];
    assert_eq!(tail, ["auto_retry_end", "agent_settled", "response"]);
    let end = &records[records.len() - 3];
    assert_eq!(end["finalError"], "Retry cancelled");
    assert_eq!(records[records.len() - 2]["aborted"], true);
    fake.close_stdin();
    fake.wait_exit().await;
}

#[tokio::test]
async fn sighup_exits_like_pi() {
    let sandbox = Sandbox::new();
    let mut fake = sandbox.spawn("{}", &[]);
    fake.call("1", json!({"type": "get_state"})).await;
    fake.signal("HUP");
    assert_eq!(fake.wait_exit().await, (Some(129), None));
}
