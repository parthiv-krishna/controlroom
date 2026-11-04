//! Acceptance (design §1, §4, spec §9, §11): real pi 1.2.0 through the `controlroom-pi`
//! bundle's launcher (with @controlroom/pi from `extensions/pi`), under nono 0.80.0 at level
//! strict, through `controlroom-agentd serve`, against the fixture tool's mock model, with the
//! harness's fake controller playing `devbox`'s host:
//!
//! 1. the session is created on `devbox:<work>`: the agent daemon opens the workspace and its
//!    default shell on the host before the agent starts;
//! 2. pi runs on the server in an empty working directory with `--no-builtin-tools
//!    --no-context-files --no-skills`; its extension reaches the agent socket from inside the
//!    sandbox and adds a todo;
//! 3. the agent starts a background job with the `job` tool: the agent daemon runs `job_run`
//!    on the workspace's host through the host bridge and the tool result names the job;
//! 4. when the extension registers the remote file tools (R1d), the agent reads
//!    `devbox:<work>/notes.txt` and gets the host's file; otherwise this step is skipped (and
//!    said so).
//!
//! Needs nono 0.80.0, Landlock and pi, so it is `#[ignore]`d; run it with
//! `crates/agentd/scripts/acceptance-pi-nono.sh`. Every process it starts is killed at the end,
//! pass or fail.

#[allow(dead_code)]
#[path = "serve/harness.rs"]
mod harness;

#[allow(dead_code)]
#[path = "serve/controller.rs"]
mod controller;

use std::path::{Path, PathBuf};
use std::time::Duration;

use controlroom_fixture_tool::mock_model::{MockModel, Reply};
use harness::{Env, op, text};
use serde_json::{Value, json};

fn env_var(var: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| {
        panic!("{var} is not set; run crates/agentd/scripts/acceptance-pi-nono.sh")
    })
}

/// The bundle's launcher, pointing the mock provider at nono's reverse proxy: nono hands the
/// sandbox the proxied base URL and a phantom key for the route.
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

/// The agent daemon's pi agent dir (the mock provider), agents.toml and sandbox.toml.
fn configure(env: &Env, mock: &MockModel, bundle: &Path, nono_tmp: &Path) {
    let agent_dir = env.state().join("agent");
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
    let config = env.state().join("config");
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
allow_loopback_upstreams = true
[[routes]]
provider = "mock"
upstream = "http://127.0.0.1:{port}"
allow = ["POST /v1/chat/completions"]
"#,
            nono = env_var("CR_TEST_NONO"),
            tmp = nono_tmp.display(),
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

/// The names of the tools pi offered the model in request `i`.
fn tools(requests: &[Value], i: usize) -> Vec<String> {
    requests[i]["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs nono 0.80.0, Landlock and pi; run crates/agentd/scripts/acceptance-pi-nono.sh"]
async fn real_pi_runs_on_the_server_and_works_on_the_hosts() {
    let root = PathBuf::from(env_var("CR_TEST_ROOT"));
    let mock = MockModel::start(vec![
        Reply::tool_call(
            "call_plan",
            "todo",
            json!({"action": "add", "items": ["build on devbox"]}),
        ),
        Reply::tool_call(
            "call_job",
            "job",
            json!({"action": "run", "command": "make all", "name": "build"}),
        ),
        Reply::text("The build runs on devbox."),
    ])
    .await
    .expect("mock model");
    let bundle = PathBuf::from(env_var("CR_TEST_PI_BUNDLE"));
    // Declared before the env, so it is dropped (and cleaned) after the env killed nono.
    let nono_tmp = NonoTmp::new();
    let mut env = Env::new_in(&root);
    configure(&env, &mock, &bundle, nono_tmp.0.path());
    // The login shell runs inside the sandbox, so it must be readable there.
    std::fs::remove_file(env.path().join("login-shell")).unwrap();
    std::os::unix::fs::symlink("/bin/sh", env.path().join("login-shell")).unwrap();
    std::fs::write(Path::new(&env.work()).join("notes.txt"), "remote notes\n").unwrap();
    env.start();

    let client = env.client();
    client.watch_sessions();
    let created = client.call(
        "session.create",
        json!({"op_id": op(), "workspaces": env.workspaces(),
               "prompt": {"content": text("Plan the build and start it.")}}),
    );
    let id = created["session"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["session"]["sandbox"]["level"], "strict");
    assert_eq!(created["session"]["workspaces"][0]["root"], env.work());

    let s = client.wait_session(&id, "the first turn", |s| {
        s["state"] == "idle"
            && s["preview"]
                .as_str()
                .is_some_and(|p| p.contains("runs on devbox"))
    });
    assert_eq!(s["todo"]["total"], 1, "{s}");

    // pi ran on the server, in the session's empty scratch directory, without local tools.
    let records = env.records(&id);
    let controlroom_transcript::Body::Meta(meta) = &records[0].body else {
        panic!("the transcript starts with its meta record");
    };
    assert!(meta.argv[0].ends_with("/nono"), "{:?}", meta.argv);
    let launched = meta.argv.join(" ");
    for flag in ["--no-builtin-tools", "--no-context-files", "--no-skills"] {
        assert!(launched.contains(flag), "{launched}");
    }
    let workdir = env.state().join(format!("sessions/{id}/work"));
    assert_eq!(std::fs::read_dir(&workdir).unwrap().count(), 0);

    // The job ran on devbox's host, through the host bridge.
    let jobs = client.call("job.list", json!({"id": id}))["jobs"].clone();
    assert_eq!(jobs[0]["name"], "build", "{jobs}");
    assert_eq!(
        jobs[0]["machine_id"],
        env.workspaces()[0]["machine_id"],
        "{jobs}"
    );
    let methods = env.controller().methods();
    for method in ["workspace.open", "shell.open", "tool.run"] {
        assert!(methods.iter().any(|m| m == method), "{method}: {methods:?}");
    }
    let requests = mock.requests();
    let offered = tools(&requests, 0);
    assert!(offered.iter().any(|t| t == "job"), "{offered:?}");

    // With the extension's remote tools, a read goes to devbox.
    if offered.iter().any(|t| t == "read") {
        mock.push([
            Reply::tool_call(
                "call_read",
                "read",
                json!({"path": format!("devbox:{}/notes.txt", env.work())}),
            ),
            Reply::text("Read the notes."),
        ]);
        let mark = client.turn_mark(&id);
        client.prompt(&id, "Read the notes.");
        client.wait_turn(&id, mark);
        let requests = mock.requests();
        let last = requests[requests.len() - 1]["messages"].to_string();
        assert!(last.contains("remote notes"), "{last}");
    } else {
        eprintln!("the extension has no remote file tools yet (R1d): read step skipped");
    }

    client.call("session.close", json!({"op_id": op(), "id": id}));
    client.wait_session(&id, "closed", |s| s["state"] == "closed");
}
