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
    let mut env = Env::new_in(&root);
    let nono_tmp = NonoTmp(
        tempfile::Builder::new()
            .prefix("crn")
            .tempdir_in("/tmp")
            .unwrap(),
    );
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
