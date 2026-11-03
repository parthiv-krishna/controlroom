//! Recovery and faults: the daemon killed mid-turn, the agent or its supervisor dying, a dormant
//! session resuming lazily, and SIGTERM leaving agents running.

use controlroom_transcript::Body;
use rustix::process::{Pid, Signal, kill_process};
use serde_json::json;

use crate::harness::*;
use crate::sessions::alive;

/// SIGKILLs `pid` if it still runs: an agent may exit by itself once its supervisor is gone
/// (its stdin closes), and an exited process is gone from the table once reaped.
fn kill(pid: u32) {
    match kill_process(Pid::from_raw(pid as i32).unwrap(), Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
        Err(e) => panic!("killing {pid}: {e}"),
    }
}

/// Whether the transcript shows pi settling a run `n` times.
fn settled_runs(env: &Env, id: &str) -> usize {
    env.records(id)
        .iter()
        .filter(|r| matches!(&r.body, Body::Out(l) if l.raw.contains("\"agent_settled\"")))
        .count()
}

#[test]
fn daemon_killed_mid_turn_loses_nothing() {
    let mut env = Env::with(json!({"replies": [
        gated("Part one", "mid", " and two."),
        {"content": [{"type": "text", "text": "Second answer."}]},
    ]}));
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    client.prompt(&id, "first");
    env.wait_gate("mid");
    let queue_id = client.prompt(&id, "second")["queue_id"].clone();
    assert!(queue_id.is_string());

    env.kill_daemon();
    client.wait_closed();
    env.start();
    let client = env.client();
    client.watch_machine();
    // Reattached, mid-run, with the queued prompt still waiting.
    let s = client.wait_session(&id, "reattached", |s| s["state"] == "working");
    assert_eq!(s["queue"]["len"], 1);
    env.release("mid");
    client.wait_session(&id, "both turns", |s| {
        s["state"] == "idle"
            && s["queue"]["len"] == 0
            && s["preview"].as_str().unwrap().contains("Second answer")
    });
    assert_eq!(env.prompts_sent(&id), ["first", "second"]);
    let (_, ops) = client.session_ops(&id, 0);
    let agent: Vec<&str> = ops
        .iter()
        .filter(|op| op["item"]["kind"] == "message" && op["item"]["role"] == "agent")
        .filter_map(|op| op["item"]["content"][0]["text"].as_str())
        .collect();
    assert_eq!(agent, ["Part one and two.", "Second answer."]);
}

#[test]
fn a_run_finishing_while_the_daemon_is_down_is_picked_up() {
    let mut env = Env::with(json!({"replies": [
        gated("Part one", "mid", " and two."),
        {"content": [{"type": "text", "text": "Second answer."}]},
    ]}));
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    client.prompt(&id, "first");
    env.wait_gate("mid");
    client.prompt(&id, "second");
    env.kill_daemon();
    env.release("mid");
    wait_until("the run to settle while the daemon is down", || {
        settled_runs(&env, &id) == 1
    });
    env.start();
    let client = env.client();
    client.watch_machine();
    client.wait_session(&id, "the queued prompt answered", |s| {
        s["state"] == "idle"
            && s["queue"]["len"] == 0
            && s["preview"].as_str().unwrap().contains("Second answer")
    });
    assert_eq!(env.prompts_sent(&id), ["first", "second"]);
}

#[test]
fn agent_crash_mid_turn_is_interrupted() {
    let env = Env::with(json!({"replies": [
        {"content": [{"type": "text", "text": "Partial"}, {"type": "crash", "code": 3}]},
    ]}));
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    client.prompt(&id, "go");
    let s = client.wait_session(&id, "interrupted", |s| s["state"] == "interrupted");
    assert!(s["last_error"].as_str().unwrap().contains('3'), "{s}");
    assert_eq!(s["resumable"], true);
}

#[test]
fn supervisor_killed_mid_turn_is_interrupted() {
    let env = Env::with(json!({"replies": [gated("Working", "hold", ".")]}));
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    client.prompt(&id, "go");
    env.wait_gate("hold");
    let (supervisor, agent) = env.supervisor_pids(&id);
    kill(supervisor);
    kill(agent);
    let s = client.wait_session(&id, "interrupted", |s| s["state"] == "interrupted");
    assert!(s["last_error"].as_str().is_some());
}

#[test]
fn dormant_session_resumes_lazily_on_prompt() {
    let mut env = Env::with(texts(&["First.", "Resumed."]));
    let client = env.client();
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    client.prompt(&id, "one");
    client.wait_session(&id, "the first turn", |s| s["last_outcome"] == "completed");

    // SIGTERM leaves the agent running; then the agent goes away while no daemon runs.
    let (supervisor, agent) = env.supervisor_pids(&id);
    assert!(env.stop_daemon().success());
    assert!(
        alive(supervisor) && alive(agent),
        "agents outlive the daemon"
    );
    kill(supervisor);
    kill(agent);
    wait_until("the agent to exit", || !alive(supervisor) && !alive(agent));

    env.start();
    let client = env.client();
    client.watch_machine();
    client.wait_session(&id, "dormant", |s| s["state"] == "dormant");
    assert_eq!(client.prompt(&id, "two")["outcome"], "queued");
    client.wait_session(&id, "the resumed turn", |s| {
        s["state"] == "idle"
            && s["queue"]["len"] == 0
            && s["preview"].as_str().unwrap().contains('.')
            && s["last_turn_end_seq"].as_u64() > Some(0)
            && env.prompts_sent(&id).len() == 2
    });
    // The second process resumed the first one's pi session file.
    let metas: Vec<Vec<String>> = env
        .records(&id)
        .into_iter()
        .filter_map(|r| match r.body {
            Body::Meta(meta) => Some(meta.argv.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(metas.len(), 2);
    assert!(
        metas[1].iter().any(|a| a.contains("--session ")),
        "{metas:?}"
    );
}
