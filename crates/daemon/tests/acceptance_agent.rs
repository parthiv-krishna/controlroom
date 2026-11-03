//! Acceptance (spec §8, §9, §14): real pi 1.2.0 through the `controlroom-pi` bundle's launcher,
//! which loads `@controlroom/pi` from the bundle (built from `extensions/pi`), under nono 0.80.0
//! at level strict, through `controlroomd serve`, against the fixture tool's mock model scripting
//! the tool calls:
//!
//! 1. the agent adds two todos and ends its turn;
//! 2. auto-continue starts the next run, in which the agent asks a question and ends its turn
//!    (the open question, not auto-continue, will wake it);
//! 3. the user answers; the answer wakes the agent, which completes both todos (by their
//!    numbers) and finishes with no open items left, so nothing continues.
//!
//! A second scenario runs a background job (a `./run-tests.sh` script in the cwd) with the `job`
//! tool, follows it with a pattern, is woken by the matching line, reads the log in its sandbox
//! and is woken by the exit.
//!
//! The extension reaches its own agent socket from inside the sandbox with the launch's token.
//! The bundle lives in the Nix store, which the sandbox reads; its launcher runs from
//! `sandbox.toml` `exec_dirs` (`CR_TEST_PI_BUNDLE`, built by the script).
//! Needs nono 0.80.0, Landlock and pi, so it is `#[ignore]`d; run it with
//! `crates/daemon/scripts/acceptance-pi-nono.sh`. Every process it starts is killed at the end,
//! pass or fail.

#[allow(dead_code)]
#[path = "serve/harness.rs"]
mod harness;

use std::path::{Path, PathBuf};
use std::time::Duration;

use controlroom_fixture_tool::mock_model::{MockModel, Reply};
use harness::{Env, op, text};
use serde_json::{Value, json};

fn env_var(var: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| {
        panic!("{var} is not set; run crates/daemon/scripts/acceptance-pi-nono.sh")
    })
}

/// The bundle's launcher (pi with @controlroom/pi), pointing the mock provider at nono's reverse
/// proxy (see `acceptance_serve.rs`).
fn agent_command(bundle: &Path, mock: &MockModel) -> String {
    let origin = format!("http://127.0.0.1:{}", mock.port());
    let placeholder = controlroom_sandbox::agent_dir::PLACEHOLDER;
    let launcher = bundle.join("bin/controlroom-pi");
    let script = format!(
        r##"m="$PI_CODING_AGENT_DIR/models.json"
sed -e "s#{origin}#$CR_MOCK_BASE_URL#" -e 's#"{placeholder}"#"$CR_MOCK_PHANTOM"#' "$m" > "$m.tmp" && mv "$m.tmp" "$m" || exit 1
PI_OFFLINE=1 PI_SKIP_VERSION_CHECK=1 PI_TELEMETRY=0 exec {launcher} -- "$@""##,
        launcher = launcher.display(),
    );
    format!(
        "sh -c {} pi",
        controlroom_sandbox::launch::shell_quote(&script)
    )
}

/// The user's (fake) pi config, agents.toml and sandbox.toml in the environment's HOME.
fn configure(env: &Env, mock: &MockModel, bundle: &Path, nono_tmp: &Path) {
    let home = env.path();
    let agent_dir = home.join(".pi/agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("models.json"),
        json!({"providers": {"mock": {
            "baseUrl": mock.base_url(),
            "api": "openai-completions",
            "apiKey": "sk-fake-test-key",
            "models": [{"id": "mock-model", "name": "Mock", "input": ["text"],
                        "contextWindow": 128000, "maxTokens": 4096,
                        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}}]
        }}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        agent_dir.join("settings.json"),
        json!({"defaultProvider": "mock", "defaultModel": "mock-model", "quietStartup": true})
            .to_string(),
    )
    .unwrap();
    let config = home.join(".config/controlroom");
    std::fs::create_dir_all(&config).unwrap();
    let command = agent_command(bundle, mock);
    std::fs::write(
        config.join("agents.toml"),
        format!("[agents.pi]\ncommand = {command:?}\ndriver = \"pi\"\n"),
    )
    .unwrap();
    std::fs::write(
        config.join("sandbox.toml"),
        format!(
            r#"nono = "{nono}"
nono_tmp = "{tmp}"
exec_dirs = ["/usr/bin", "/bin", "{bundle}/bin", "{bundle}/libexec/controlroom-pi/tools"]
allow_loopback_upstreams = true
[[routes]]
provider = "mock"
upstream = "http://127.0.0.1:{port}"
allow = ["POST /v1/chat/completions"]
"#,
            nono = env_var("CR_TEST_NONO"),
            tmp = nono_tmp.display(),
            bundle = bundle.display(),
            port = mock.port(),
        ),
    )
    .unwrap();
}

/// nono's short `TMPDIR`; removes what a killed nono left when dropped (after the env).
struct NonoTmp(tempfile::TempDir);

impl NonoTmp {
    fn new() -> Self {
        NonoTmp(
            tempfile::Builder::new()
                .prefix("crn")
                .tempdir_in("/tmp")
                .unwrap(),
        )
    }
}

impl Drop for NonoTmp {
    fn drop(&mut self) {
        controlroom_sandbox::tempdirs::clean_leaked_nono_dirs(self.0.path(), Duration::ZERO);
    }
}

/// The text of every message pi sent the model in request `i`.
fn conversation(requests: &[Value], i: usize) -> String {
    requests[i]["messages"].to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs nono 0.80.0, Landlock and pi; run crates/daemon/scripts/acceptance-pi-nono.sh"]
async fn real_pi_with_the_extension_adds_todos_continues_asks_and_is_woken_by_the_answer() {
    let root = PathBuf::from(env_var("CR_TEST_ROOT"));
    let mock = MockModel::start(vec![
        Reply::tool_call(
            "call_plan",
            "todo",
            json!({"action": "add", "items": ["migrate the schema", "write the docs"]}),
        ),
        Reply::text("Planned."),
        Reply::tool_call(
            "call_ask",
            "ask_user",
            json!({"questions": [{"question": "Which database?", "options": ["Postgres", "SQLite"]}]}),
        ),
        Reply::text("Waiting for the answer."),
        Reply::tool_call("call_one", "todo", json!({"action": "complete", "id": 1})),
        Reply::tool_call("call_two", "todo", json!({"action": "complete", "id": "2"})),
        Reply::text("All done."),
    ])
    .await
    .expect("mock model");
    let bundle = PathBuf::from(env_var("CR_TEST_PI_BUNDLE"));
    assert!(
        bundle
            .join("share/controlroom-pi/extension/src/index.ts")
            .exists(),
        "the bundle carries the real @controlroom/pi"
    );
    // Declared before the env, so it is dropped (and cleaned) after the env killed nono.
    let nono_tmp = NonoTmp::new();
    let mut env = Env::new_in(&root);
    configure(&env, &mock, &bundle, nono_tmp.0.path());
    // The login shell runs inside the sandbox, so it must be readable there.
    std::fs::remove_file(env.path().join("login-shell")).unwrap();
    std::os::unix::fs::symlink("/bin/sh", env.path().join("login-shell")).unwrap();
    env.start();

    let client = env.client();
    client.watch_machine();
    let created = client.call(
        "session.create",
        json!({"op_id": op(), "agent": "pi", "cwd": env.work(),
               "prompt": {"content": text("Plan the migration and do it.")}}),
    );
    let id = created["session"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["session"]["sandbox"]["level"], "strict");

    // Steps 1 and 2: two todos, a continue run, an open question; then the agent rests.
    let s = client.wait_session(&id, "the open question", |s| {
        s["state"] == "idle"
            && s["requests"]
                .as_array()
                .is_some_and(|r| r.iter().any(|r| r["kind"] == "question"))
    });
    assert_eq!(s["attention"], "question", "{s}");
    assert_eq!(s["todo"]["total"], 2, "{s}");
    assert_eq!(s["todo"]["done"], 0, "{s}");
    let question = s["requests"][0].clone();
    assert_eq!(question["title"], "Which database?");
    assert_eq!(question["blocking"], false);
    assert_eq!(question["questions"][0]["options"][0]["label"], "Postgres");
    let requests = mock.requests();
    assert_eq!(requests.len(), 4, "the question doesn't start another run");
    // The extension's system prompt section reached the model.
    assert!(
        conversation(&requests, 0).contains("You run inside controlroom"),
        "{}",
        conversation(&requests, 0)
    );
    // The continue run's prompt lists the open items; the ask returned the reminder.
    assert!(conversation(&requests, 2).contains("Auto-continue"));
    assert!(conversation(&requests, 3).contains("the answer will wake you"));

    // Step 3: the answer wakes the agent; it completes both items and stops.
    client.call(
        "request.respond",
        json!({"op_id": op(), "id": id, "request_id": question["id"],
               "response": {"kind": "answers", "answers": [
                   {"question_id": "q1", "selected": ["Postgres"]}]},
               "by": {"device_id": "laptop-1", "label": "Laptop"}}),
    );
    let s = client.wait_session(&id, "all done", |s| {
        s["state"] == "idle"
            && s["todo"]["done"] == 2
            && s["preview"]
                .as_str()
                .is_some_and(|p| p.contains("All done"))
    });
    assert!(s["requests"].as_array().unwrap().is_empty());
    let requests = mock.requests();
    assert_eq!(requests.len(), 7, "no continue run after the last item");
    assert!(
        conversation(&requests, 4).contains("Postgres"),
        "{}",
        conversation(&requests, 4)
    );

    let (_, ops) = client.session_ops(&id, 0);
    let triggers: Vec<Value> = ops
        .iter()
        .filter(|op| op["item"]["kind"] == "run")
        .map(|op| op["item"]["trigger"].clone())
        .collect();
    let kinds: Vec<&str> = triggers
        .iter()
        .map(|t| t["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["user", "auto_continue", "inject"], "{triggers:#?}");
    assert_eq!(triggers[2]["origin"], "answer");
    let todos =
        client.call("session.subscribe", json!({"id": id, "since_seq": 0}))["detail"]["todos"]
            .clone();
    for todo in todos.as_array().unwrap() {
        assert_eq!(todo["status"], "done", "{todos}");
        assert_eq!(todo["updated_by"]["kind"], "agent", "{todos}");
    }

    // Closing stops pi; nono leaves no temp dirs behind.
    client.call("session.close", json!({"op_id": op(), "id": id}));
    client.wait_session(&id, "closed", |s| s["state"] == "closed");
}

/// Writes an executable file from a child shell (a test process forked while this process held
/// the file open for writing would make executing it fail with ETXTBSY).
fn write_executable(path: &Path, content: &str) {
    let status = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"printf '%s' "$1" > "$2" && chmod 755 "$2""#)
        .arg("sh")
        .arg(content)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

/// The job's FIFO gates: the job blocks reading one until the test writes it.
fn mkfifo(path: &Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

/// Acceptance for jobs (spec §8, §9, P5 "a job-triggered continuation works"): under nono
/// strict, the agent starts a job with the `job` tool, follows it with a pattern, is woken by
/// the matching line, reads the log in its sandbox, and is woken again by the exit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs nono 0.80.0, Landlock and pi; run crates/daemon/scripts/acceptance-pi-nono.sh"]
async fn real_pi_runs_follows_and_is_woken_by_a_job() {
    let root = PathBuf::from(env_var("CR_TEST_ROOT"));
    let nono_tmp = NonoTmp::new();
    let mut env = Env::new_in(&root);
    let (fail, finish) = (
        Path::new(&env.work()).join("fail.fifo"),
        Path::new(&env.work()).join("finish.fifo"),
    );
    mkfifo(&fail);
    mkfifo(&finish);
    // A test script in the cwd, run as `./run-tests.sh` through `/usr/bin/env bash` (the session's
    // cwd and `exec_dirs` are executable under nono strict).
    let script = format!(
        "#!/usr/bin/env bash\necho \"building with bash ${{BASH_VERSINFO[0]}}\"\nread a < {}\necho 'FAIL test_login: expected 200'\nread b < {}\nexit 1\n",
        fail.display(),
        finish.display()
    );
    write_executable(&Path::new(&env.work()).join("run-tests.sh"), &script);
    let command = "./run-tests.sh";
    let mock = MockModel::start(vec![
        Reply::tool_call(
            "call_run",
            "job",
            json!({"action": "run", "command": command, "name": "suite"}),
        ),
        Reply::text("The suite runs in the background."),
    ])
    .await
    .expect("mock model");
    let bundle = PathBuf::from(env_var("CR_TEST_PI_BUNDLE"));
    configure(&env, &mock, &bundle, nono_tmp.0.path());
    std::fs::remove_file(env.path().join("login-shell")).unwrap();
    std::os::unix::fs::symlink("/bin/sh", env.path().join("login-shell")).unwrap();
    env.start();

    let client = env.client();
    client.watch_machine();
    let created = client.call(
        "session.create",
        json!({"op_id": op(), "agent": "pi", "cwd": env.work(),
               "prompt": {"content": text("Run the test suite in the background.")}}),
    );
    let id = created["session"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["session"]["sandbox"]["level"], "strict");

    // 1. The job runs under the session's sandbox; the turn ends while it runs.
    let s = client.wait_session(&id, "the job running, the agent idle", |s| {
        s["state"] == "idle" && s["live_work"]["jobs"] == 1 && s["last_turn_end_seq"] != 0
    });
    assert_eq!(s["live_work"]["jobs"], 1);
    let jobs = client.call("job.list", json!({"id": id}))["jobs"].clone();
    let job_id = jobs[0]["id"].as_str().unwrap().to_owned();
    let log = jobs[0]["log_path"].as_str().unwrap().to_owned();
    assert_eq!(
        log,
        format!("{}/.controlroom/jobs/{job_id}.log", env.work())
    );
    assert!(
        conversation(&mock.requests(), 1).contains(&log),
        "the tool result names the log"
    );
    // Its supervisor runs it under nono with the session's profile.
    let job_transcript = env
        .state()
        .join(format!("sessions/{id}/jobs/{job_id}/transcript.jsonl"));
    let meta = controlroom_transcript::read_all(&job_transcript, 0)
        .unwrap()
        .records[0]
        .clone();
    let controlroom_transcript::Body::Meta(meta) = meta.body else {
        panic!("the job transcript starts with its meta record");
    };
    assert!(meta.argv[0].ends_with("/nono"), "{:?}", meta.argv);
    assert!(
        meta.argv.iter().any(|a| a.ends_with("profile.json")),
        "{:?}",
        meta.argv
    );

    // 2. The user asks it to watch for failures: it follows the job with a pattern.
    mock.push([
        Reply::tool_call(
            "call_follow",
            "job",
            json!({"action": "follow", "job_id": job_id, "pattern": "FAIL", "buffer_seconds": 1}),
        ),
        Reply::text("Following the suite for failures."),
    ]);
    let mark = client.turn_mark(&id);
    client.prompt(&id, "Tell me as soon as a test fails.");
    client.wait_turn(&id, mark);
    let job = client.call("job.list", json!({"id": id}))["jobs"][0].clone();
    assert_eq!(job["follow"]["pattern"], "FAIL", "{job}");

    // 3. A failing line wakes it (after the buffer); it reads the log in its sandbox.
    mock.push([
        Reply::tool_call(
            "call_read",
            "bash",
            json!({"command": format!("grep -n FAIL {log}")}),
        ),
        Reply::text("test_login fails with a 500."),
    ]);
    let mark = client.turn_mark(&id);
    std::fs::write(&fail, b"go\n").unwrap();
    client.wait_turn(&id, mark);
    let requests = mock.requests();
    // The wake is the last message of the request that answered it (the job's command, which
    // the conversation also holds, contains the failing line's text).
    let woken = requests[requests.len() - 2]["messages"]
        .as_array()
        .and_then(|m| m.last())
        .unwrap()
        .to_string();
    assert!(
        woken.contains("[controlroom] Background job update"),
        "{woken}"
    );
    assert!(woken.contains("matching /FAIL/ at line 2"), "{woken}");
    assert!(
        !woken.contains("expected 200"),
        "the wake carries no log content: {woken}"
    );
    let read = conversation(&requests, requests.len() - 1);
    assert!(read.contains("2:FAIL test_login: expected 200"), "{read}");
    // The script's `/usr/bin/env bash` interpreter ran in the sandbox.
    let first = std::fs::read_to_string(&log).unwrap();
    assert!(first.starts_with("building with bash "), "{first}");

    // 4. The exit wakes it once more.
    mock.push([Reply::text("The suite failed; test_login needs a fix.")]);
    let mark = client.turn_mark(&id);
    std::fs::write(&finish, b"go\n").unwrap();
    let s = client.wait_session(&id, "the exit's run", |s| {
        s["state"] == "idle"
            && s["live_work"]["jobs"] == 0
            && s["last_turn_end_seq"].as_u64() > Some(mark)
            && s["preview"]
                .as_str()
                .is_some_and(|p| p.contains("needs a fix"))
    });
    assert_eq!(s["live_work"]["jobs"], 0);
    let requests = mock.requests();
    let exit = conversation(&requests, requests.len() - 1);
    assert!(exit.contains("exited with code 1"), "{exit}");

    let (_, ops) = client.session_ops(&id, 0);
    let triggers: Vec<Value> = ops
        .iter()
        .filter(|op| op["item"]["kind"] == "run")
        .map(|op| op["item"]["trigger"].clone())
        .collect();
    let labels: Vec<String> = triggers
        .iter()
        .map(|t| match t["kind"].as_str().unwrap() {
            "inject" => t["label"].as_str().unwrap().to_owned(),
            kind => kind.to_owned(),
        })
        .collect();
    assert_eq!(
        labels,
        ["user", "user", "job: suite (follow)", "job: suite (exit 1)"],
        "{triggers:#?}"
    );

    client.call("session.close", json!({"op_id": op(), "id": id}));
    client.wait_session(&id, "closed", |s| s["state"] == "closed");
}
