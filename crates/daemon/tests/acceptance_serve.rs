//! Acceptance: a real `pi --mode rpc` session driven **through `controlroomd serve`**,
//! sandboxed by nono 0.80.0 with fake credentials only, against the fixture tool's local mock
//! model.
//!
//! - The daemon is SIGKILLed mid-run and restarted; the run completes and no prompt is sent
//!   twice.
//! - pi runs a mediated `git push` (to a bare repo in the working directory): nono asks the
//!   daemon's approval webhook, the push shows up as a `sandbox` request with remote, branch
//!   and force, and the user's answer decides it: allowed, the branch arrives; denied with a
//!   reason, it doesn't and the command fails.
//!
//! Needs nono 0.80.0, Landlock and pi, so it is `#[ignore]`d. Run it with
//! `crates/daemon/scripts/acceptance-pi-nono.sh` (see `acceptance_pi_nono.rs` for the variables
//! it sets). The scratch environment lives under `CR_TEST_ROOT` (nono refuses a state dir under
//! `/tmp`), and every process it starts is killed at the end, pass or fail.

#[allow(dead_code)]
#[path = "serve/harness.rs"]
mod harness;

use std::path::Path;
use std::time::Duration;

use controlroom_fixture_tool::mock_model::{MockModel, Reply};
use controlroom_transcript::Body;
use harness::{Env, op, text};
use serde_json::{Value, json};

const GATE_TIMEOUT: Duration = Duration::from_secs(120);

fn env_var(var: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| {
        panic!("{var} is not set; run crates/daemon/scripts/acceptance-pi-nono.sh")
    })
}

/// The pi agent command, pointing the mock provider at nono's reverse proxy first (see
/// `agent_command` in `acceptance_pi_nono.rs` for why a loopback upstream needs this).
fn agent_command(pi: &str, mock: &MockModel) -> String {
    let origin = format!("http://127.0.0.1:{}", mock.port());
    let placeholder = controlroom_sandbox::agent_dir::PLACEHOLDER;
    let script = format!(
        r##"m="$PI_CODING_AGENT_DIR/models.json"
sed -e "s#{origin}#$CR_MOCK_BASE_URL#" -e 's#"{placeholder}"#"$CR_MOCK_PHANTOM"#' "$m" > "$m.tmp" && mv "$m.tmp" "$m" || exit 1
PI_OFFLINE=1 PI_SKIP_VERSION_CHECK=1 PI_TELEMETRY=0 exec {pi} "$@""##
    );
    format!(
        "sh -c {} pi",
        controlroom_sandbox::launch::shell_quote(&script)
    )
}

/// The user's (fake) pi config, agents.toml and sandbox.toml in the environment's HOME.
fn configure(env: &Env, mock: &MockModel, nono_tmp: &Path) {
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
    let command = agent_command(&env_var("CR_TEST_PI"), mock);
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

/// Runs `git` host-side (test setup and checks), failing the test if it fails.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// nono's `TMPDIR` for one test. Short, because nono binds sockets in it (`NONO_TMP_MAX`).
/// Dropped after the environment (whose drop kills nono), it removes what a killed nono left,
/// read-only shims included, so a failed run leaves nothing in `/tmp`.
struct NonoTmp(tempfile::TempDir);

impl NonoTmp {
    fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for NonoTmp {
    fn drop(&mut self) {
        controlroom_sandbox::tempdirs::clean_leaked_nono_dirs(self.path(), Duration::ZERO);
    }
}

/// A started environment with the pi agent under nono, and its nono `TMPDIR`. Bind them as
/// `(nono_tmp, env)` so the environment is dropped first.
fn sandboxed_env(root: &str, mock: &MockModel) -> (NonoTmp, Env) {
    let mut env = Env::new_in(Path::new(root));
    let nono_tmp = NonoTmp(
        tempfile::Builder::new()
            .prefix("crn")
            .tempdir_in("/tmp")
            .unwrap(),
    );
    configure(&env, mock, nono_tmp.path());
    // The login shell runs inside the sandbox, so it must be readable there (the harness's
    // stand-in lives in the scratch dir).
    std::fs::remove_file(env.path().join("login-shell")).unwrap();
    std::os::unix::fs::symlink("/bin/sh", env.path().join("login-shell")).unwrap();
    env.start();
    (nono_tmp, env)
}

fn nono_dirs(tmp: &Path) -> Vec<String> {
    std::fs::read_dir(tmp)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("nono-"))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs nono 0.80.0, Landlock and pi; run crates/daemon/scripts/acceptance-pi-nono.sh"]
async fn real_pi_through_serve_under_nono_survives_a_daemon_kill() {
    let root = env_var("CR_TEST_ROOT");
    let mock = MockModel::start(vec![
        Reply::gated_text("Part one", "mid", " and two."),
        Reply::text("Second answer."),
    ])
    .await
    .expect("mock model");
    let (nono_tmp, mut env) = sandboxed_env(&root, &mock);

    let client = env.client();
    client.watch_machine();
    let created = client.call(
        "session.create",
        json!({"op_id": op(), "agent": "pi", "cwd": env.work()}),
    );
    let id = created["session"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["session"]["sandbox"]["level"], "strict");
    client.wait_idle(&id);

    client.prompt(&id, "first");
    tokio::time::timeout(GATE_TIMEOUT, mock.wait_reached("mid", GATE_TIMEOUT))
        .await
        .expect("the model gate")
        .expect("the model reached the gate");
    let queued = client.call(
        "session.prompt",
        json!({"op_id": op(), "id": id, "content": text("second")}),
    );
    assert_eq!(queued["outcome"], "queued", "{queued}");

    // SIGKILL the daemon mid-run; the supervisor and pi keep going.
    env.kill_daemon();
    client.wait_closed();
    env.start();
    let client = env.client();
    client.watch_machine();
    mock.release("mid");
    let done = client.wait_session(&id, "both turns", |s| {
        s["state"] == "idle"
            && s["queue"]["len"] == 0
            && s["preview"]
                .as_str()
                .is_some_and(|p| p.contains("Second answer"))
    });
    assert_eq!(done["last_outcome"], "completed");

    // Each prompt went to pi once, and pi asked the model once per prompt.
    assert_eq!(env.prompts_sent(&id), ["first", "second"]);
    assert_eq!(mock.requests().len(), 2);
    let (_, ops) = client.session_ops(&id, 0);
    let agent: Vec<String> = ops
        .iter()
        .filter(|op| op["item"]["kind"] == "message" && op["item"]["role"] == "agent")
        .filter_map(|op| op["item"]["content"][0]["text"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(agent, ["Part one and two.", "Second answer."]);
    // stdout stayed clean JSONL.
    for record in env.records(&id) {
        if let Body::Out(line) = &record.body {
            serde_json::from_str::<Value>(&line.raw)
                .unwrap_or_else(|e| panic!("non-JSON stdout line {:?}: {e}", line.raw));
        }
    }

    close_without_leaks(&env, &client, &id, nono_tmp.path());
}

/// Closing stops pi; nono leaves no temp dirs behind.
fn close_without_leaks(env: &Env, client: &harness::Client, id: &str, nono_tmp: &Path) {
    let (supervisor, _) = env.supervisor_pids(id);
    client.call("session.close", json!({"op_id": op(), "id": id}));
    client.wait_session(id, "closed", |s| s["state"] == "closed");
    harness::wait_until("the supervisor to exit", || {
        !std::path::Path::new(&format!("/proc/{supervisor}")).exists()
            || std::fs::read_to_string(format!("/proc/{supervisor}/stat"))
                .map(|s| s.rsplit(") ").next().unwrap_or("").starts_with('Z'))
                .unwrap_or(true)
    });
    let leaked = nono_dirs(nono_tmp);
    controlroom_sandbox::tempdirs::clean_leaked_nono_dirs(nono_tmp, Duration::ZERO);
    assert!(leaked.is_empty(), "leaked nono temp dirs: {leaked:?}");
}

/// The single pending request of `session` once it shows up.
fn pending_request(client: &harness::Client, session: &str) -> Value {
    let s = client.wait_session(session, "a sandbox approval", |s| {
        s["requests"]
            .as_array()
            .is_some_and(|r| r.iter().any(|r| r["kind"] == "sandbox"))
    });
    assert_eq!(s["state"], "awaiting_permission", "{s}");
    s["requests"][0].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs nono 0.80.0, Landlock, pi and git; run crates/daemon/scripts/acceptance-pi-nono.sh"]
async fn real_pi_git_push_needs_an_approval_through_serve() {
    let root = env_var("CR_TEST_ROOT");
    let push = |branch: &str| json!({"command": format!("cd repo && git push origin HEAD:{branch} 2>&1; echo exit=$?")});
    let mock = MockModel::start(vec![
        Reply::tool_call("call_push_1", "bash", push("allowed")),
        Reply::text("Pushed."),
        Reply::tool_call("call_push_2", "bash", push("denied")),
        Reply::text("The push was denied."),
    ])
    .await
    .expect("mock model");
    let (nono_tmp, env) = sandboxed_env(&root, &mock);

    // A bare remote and a clone with one commit, inside the working directory.
    let work = Path::new(&env.work()).to_owned();
    git(&work, &["init", "-q", "--bare", "remote.git"]);
    git(&work, &["init", "-q", "repo"]);
    let repo = work.join("repo");
    git(
        &repo,
        &[
            "-c",
            "user.email=alice@example.com",
            "-c",
            "user.name=alice",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    git(&repo, &["remote", "add", "origin", "../remote.git"]);
    let remote_has = |branch: &str| {
        std::process::Command::new("git")
            .current_dir(work.join("remote.git"))
            .args([
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{branch}"),
            ])
            .status()
            .expect("git")
            .success()
    };

    let client = env.client();
    let fresh = harness::Client::connect(&env.socket());
    let hello = fresh.hello_debug();
    assert!(
        hello["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("sandbox_approvals")),
        "{hello}"
    );
    client.watch_machine();
    let created = client.call(
        "session.create",
        json!({"op_id": op(), "agent": "pi", "cwd": env.work(),
               "prompt": {"content": text("push it")}}),
    );
    let id = created["session"]["id"].as_str().unwrap().to_owned();

    // Allowed once: the branch arrives.
    let request = pending_request(&client, &id);
    assert_eq!(request["body"]["kind"], "git_push", "{request}");
    assert_eq!(request["body"]["remote"], "origin");
    assert_eq!(request["body"]["branch"], "HEAD:allowed");
    assert_eq!(request["body"]["force"], false);
    client.call(
        "request.respond",
        json!({"op_id": op(), "id": id, "request_id": request["id"],
               "response": {"kind": "choice", "option_id": "allow-once"}}),
    );
    let done = client.wait_session(&id, "the first turn", |s| {
        s["state"] == "idle" && s["preview"].as_str().is_some_and(|p| p.contains("Pushed"))
    });
    assert!(done["requests"].as_array().unwrap().is_empty());
    assert!(remote_has("allowed"), "the allowed push reached the remote");

    // Denied with a reason: nothing arrives and the command fails.
    let mark = client.turn_mark(&id);
    client.prompt(&id, "push again");
    let request = pending_request(&client, &id);
    assert_eq!(request["body"]["branch"], "HEAD:denied");
    client.call(
        "request.respond",
        json!({"op_id": op(), "id": id, "request_id": request["id"],
               "response": {"kind": "choice", "option_id": "deny", "reason": "not this branch"}}),
    );
    client.wait_turn(&id, mark);
    assert!(
        !remote_has("denied"),
        "the denied push must not reach the remote"
    );
    // What pi told the model each push did: the last message of the request after it.
    let requests = mock.requests();
    let tool_result = |i: usize| {
        let messages = requests[i]["messages"].as_array().unwrap();
        messages.last().unwrap()["content"].to_string()
    };
    assert!(tool_result(1).contains("exit=0"), "{}", tool_result(1));
    let denied = tool_result(3);
    assert!(denied.contains("exit=126"), "{denied}");
    assert!(
        denied.contains("not this branch"),
        "the reason reaches the agent: {denied}"
    );

    close_without_leaks(&env, &client, &id, nono_tmp.path());
}
