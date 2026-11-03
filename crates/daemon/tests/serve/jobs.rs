//! Background jobs end to end: the fake agent plays `@controlroom/pi` and starts a job through
//! the agent socket; the job runs under a real supervisor, writes its plain log into the
//! session's cwd, survives a daemon restart, and its exit wakes the agent. The job waits on a
//! FIFO the test opens, so it ends exactly when the test says.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::harness::*;

fn say(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

/// A `job` tool call that calls the agent API's `job.run`.
fn run_job(command: &str) -> Value {
    let params = json!({"call_id": "call_job", "command": command, "name": "suite"});
    json!({"content": [{"type": "tool_call", "id": "call_job", "name": "job", "args": params.clone(),
        "agent": {"method": "job.run", "params": params}}]})
}

fn mkfifo(path: &Path) {
    let status = Command::new("mkfifo").arg(path).status().unwrap();
    assert!(status.success());
}

/// Lets the job past its FIFO (opening the write end unblocks its `read`).
fn release(fifo: &Path) {
    std::fs::write(fifo, b"go\n").unwrap();
}

fn job_list(client: &Client, session: &str) -> Vec<Value> {
    client.call("job.list", json!({"id": session}))["jobs"]
        .as_array()
        .unwrap()
        .clone()
}

fn wait_job(client: &Client, session: &str, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let mut found = None;
    wait_until(what, || {
        found = job_list(client, session).into_iter().find(&pred);
        found.is_some()
    });
    found.unwrap()
}

#[test]
fn a_job_logs_survives_a_daemon_restart_and_its_exit_wakes_the_agent() {
    let mut env = Env::new();
    let fifo: PathBuf = env.path().join("gates/job.fifo");
    mkfifo(&fifo);
    let command = format!(
        "printf '\\033[32mok\\033[0m 1\\n'; echo 'FAIL one'; read x < {}; echo 'FAIL two'; exit 3",
        fifo.display()
    );
    env.agent(
        "fake",
        json!({"replies": [run_job(&command), say("Started the suite."), say("Saw it fail.")]}),
    );
    env.start();
    let client = Client::connect(&env.socket());
    assert!(
        client.hello_debug()["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("jobs"))
    );
    client.watch_machine();
    let id = client.create("fake", &env.work());
    client.wait_idle(&id);
    client.prompt(&id, "run the suite in the background");

    let job = wait_job(&client, &id, "the job's first lines", |j| j["lines"] == 2);
    let job_id = job["id"].as_str().unwrap().to_owned();
    let log = format!("{}/.controlroom/jobs/{job_id}.log", env.work());
    assert_eq!(job["log_path"], json!(log));
    assert_eq!(job["status"], "running");
    assert_eq!(
        std::fs::read_to_string(Path::new(&env.work()).join(".controlroom/.gitignore")).unwrap(),
        "*\n"
    );
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "ok 1\nFAIL one\n");
    // The tool result told the agent the job id and the log path.
    let s = client.wait_session(&id, "the agent's turn ends", |s| {
        s["state"] == "idle" && s["live_work"]["jobs"] == 1
    });
    assert_eq!(s["live_work"]["jobs"], 1);

    // The user follows it from the UI (the agent usually does), then the daemon restarts.
    let followed = client.call(
        "job.follow",
        json!({"op_id": op(), "id": id, "job_id": job_id, "pattern": "FAIL", "from": "now"}),
    );
    assert_eq!(followed["follow"]["pattern"], "FAIL");
    env.kill_daemon();
    env.start();
    let client = env.client();
    client.watch_machine();
    let job = wait_job(&client, &id, "the job reattached", |j| {
        j["status"] == "running" && j["lines"] == 2
    });
    assert_eq!(job["follow"]["pattern"], "FAIL");

    // The job ends; its exit wakes the agent with metadata only.
    let mark = client.turn_mark(&id);
    release(&fifo);
    let job = wait_job(&client, &id, "the job's exit", |j| j["status"] == "failed");
    assert_eq!(
        (job["exit"].clone(), job["lines"].clone()),
        (json!(3), json!(3))
    );
    assert_eq!(job["follow"], Value::Null);
    client.wait_turn(&id, mark);
    let prompts = env.prompts_sent(&id);
    let wake = prompts
        .iter()
        .find(|p| p.contains("Background job update"))
        .unwrap_or_else(|| panic!("no wake in {prompts:#?}"));
    assert!(
        wake.contains(&format!("job {job_id} `suite` exited with code 3")),
        "{wake}"
    );
    assert!(wake.contains(&format!("3 lines in {log}")), "{wake}");
    assert!(
        !wake.contains("FAIL two"),
        "log content never reaches the agent: {wake}"
    );
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "ok 1\nFAIL one\nFAIL two\n"
    );

    // The user's view: logs from the transcript, and the timeline item.
    let logs = client.call(
        "job.logs",
        json!({"id": id, "job_id": job_id, "from_line": 2}),
    );
    assert_eq!(logs["lines"], json!(["FAIL one", "FAIL two"]));
    assert_eq!(logs["total_lines"], 3);
    let (_, ops) = client.session_ops(&id, 0);
    let item = ops
        .iter()
        .find(|op| op["item"]["kind"] == "job")
        .expect("a job item");
    assert_eq!(item["item"]["job"]["status"], "failed");
    let trigger = ops
        .iter()
        .rev()
        .find(|op| op["item"]["kind"] == "run")
        .map(|op| op["item"]["trigger"].clone())
        .unwrap();
    assert_eq!(trigger["origin"], "job");
    assert_eq!(trigger["label"], "job: suite (exit 3)");
    client.wait_session(&id, "no live work", |s| s["live_work"]["jobs"] == 0);
}
