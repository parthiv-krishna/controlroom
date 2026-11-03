//! Acceptance: a real `pi --mode rpc` session launched through the launch glue, under nono,
//! with fake credentials only, driven by raw RPC lines through its supervisor.
//!
//! Needs nono 0.80.0, Landlock and pi, so it is `#[ignore]`d. Run it with
//! `crates/daemon/scripts/acceptance-pi-nono.sh`, which builds the pinned nono, isolates `HOME`
//! and the XDG dirs under a scratch root, and sets:
//!
//! - `CR_TEST_ROOT`: the scratch root (`HOME` is inside it);
//! - `CR_TEST_NONO`: the nono binary;
//! - `CR_TEST_PI`: the pi binary.
//!
//! The model is the fixture tool's local mock; no real provider or key is involved.

use std::path::Path;
use std::time::Duration;

use controlroom_fixture_tool::mock_model::{MockModel, Reply};
use controlroom_protocol::session::DriverKind;
use controlroom_sandbox::policy::{ScopePolicy, default_command_rules};
use controlroom_sandbox::tempdirs::clean_leaked_nono_dirs;
use controlroom_sandbox::{AbsPath, SessionId, effective_policy};
use controlroom_supervisor::{Client, RecordStream};
use controlroom_transcript::{Body, Record, StopCause, read_all};
use controlroomd::config::agents::AgentEntry;
use controlroomd::config::sandbox::SandboxConfig;
use controlroomd::dirs::Dirs;
use controlroomd::launch::{self, AgentLaunch, Host};
use serde_json::{Value, json};

const STEP_TIMEOUT: Duration = Duration::from_secs(120);

fn env_path(var: &str) -> AbsPath {
    let value = std::env::var(var).unwrap_or_else(|_| {
        panic!("{var} is not set; run crates/daemon/scripts/acceptance-pi-nono.sh")
    });
    AbsPath::new(&value).unwrap_or_else(|e| panic!("{var}: {e}"))
}

fn write(path: &AbsPath, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("parent").as_path()).expect("mkdir");
    std::fs::write(path.as_path(), contents).expect("write");
}

/// The user's (fake) pi config: the mock provider with a fake key.
fn write_user_agent_dir(dir: &AbsPath, mock: &MockModel) {
    write(
        &dir.join("models.json"),
        &json!({"providers": {"mock": {
            "baseUrl": mock.base_url(),
            "api": "openai-completions",
            "apiKey": "sk-fake-test-key",
            "models": [{"id": "mock-model", "name": "Mock", "input": ["text"],
                        "contextWindow": 128000, "maxTokens": 4096,
                        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}}]
        }}})
        .to_string(),
    );
    write(
        &dir.join("settings.json"),
        &json!({"defaultProvider": "mock", "defaultModel": "mock-model", "quietStartup": true})
            .to_string(),
    );
}

/// The agent command: pi, after pointing the mock provider at nono's reverse proxy.
///
/// A loopback upstream is only reachable through the route's reverse-proxy URL
/// (`$CR_MOCK_BASE_URL`, which authenticates with the route's phantom token): pi's Node fetch
/// tunnels plain HTTP through `CONNECT`, which nono's proxy answers with TLS interception.
/// Real providers are HTTPS and need none of this. The phantom token is referenced as
/// `$CR_MOCK_PHANTOM`, which pi resolves itself, so no token is written to disk; nono swaps
/// it for the key that `controlroomd credential` reads host-side.
fn agent_command(pi: &AbsPath, mock: &MockModel) -> String {
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

/// Waits for the next stdout JSON line matching `pred`, checking every line on the way.
async fn wait_out(
    records: &mut RecordStream,
    seen: &mut Vec<Record>,
    what: &str,
    pred: impl Fn(&Value) -> bool,
) -> Value {
    let found = tokio::time::timeout(STEP_TIMEOUT, async {
        while let Some(record) = records.next().await {
            seen.push(record.clone());
            if let Body::Out(line) = &record.body {
                let value: Value = serde_json::from_str(&line.raw)
                    .unwrap_or_else(|e| panic!("non-JSON stdout line {:?}: {e}", line.raw));
                if pred(&value) {
                    return Some(value);
                }
            }
        }
        None
    })
    .await;
    match found {
        Ok(Some(value)) => value,
        Ok(None) => panic!("the transcript ended before {what}; seen: {seen:#?}"),
        Err(_) => panic!("timed out waiting for {what}; seen: {seen:#?}"),
    }
}

fn nono_dirs(tmp: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(tmp)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("nono-"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Stops the supervisor if the test fails half-way.
struct Terminate(Option<std::path::PathBuf>);

impl Drop for Terminate {
    fn drop(&mut self) {
        let Some(socket) = self.0.take() else { return };
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(async {
                if let Ok(client) = Client::connect(&socket, true).await {
                    let _ = client.terminate(Some(Duration::from_secs(1))).await;
                }
            });
        })
        .join();
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs nono 0.80.0, Landlock and pi; run crates/daemon/scripts/acceptance-pi-nono.sh"]
async fn real_pi_under_nono_through_the_launch_glue() {
    let root = env_path("CR_TEST_ROOT");
    let dirs = Dirs::from_env(None).expect("dirs");
    assert!(
        root.contains(dirs.home()),
        "HOME must be isolated inside CR_TEST_ROOT"
    );
    let pi = env_path("CR_TEST_PI");
    let cwd = root.join("work");
    let outside = root.join("outside");
    let nono_tmp = root.join("t");
    for dir in [&cwd, &outside, &nono_tmp] {
        std::fs::create_dir_all(dir.as_path()).expect("mkdir");
    }

    let outside_file = outside.join("escape.txt");
    let mock = MockModel::start(vec![
        Reply::tool_call(
            "call_write",
            "write",
            json!({"path": "inside.txt", "content": "written by pi\n"}),
        ),
        Reply::tool_call(
            "call_escape",
            "bash",
            json!({"command": format!("echo escaped > {outside_file}; echo rc=$?")}),
        ),
        Reply::text("done"),
    ])
    .await
    .expect("mock model");
    let user_agent = dirs.home().join(".pi/agent");
    write_user_agent_dir(&user_agent, &mock);

    let config: SandboxConfig = toml::from_str(&format!(
        r#"
nono_tmp = "{nono_tmp}"
allow_loopback_upstreams = true
[[routes]]
provider = "mock"
upstream = "http://127.0.0.1:{port}"
allow = ["POST /v1/chat/completions"]
"#,
        port = mock.port(),
    ))
    .expect("sandbox.toml");
    let host = Host {
        controlroomd: AbsPath::new(env!("CARGO_BIN_EXE_controlroomd")).expect("bin path"),
        shell: AbsPath::new("/bin/sh").expect("shell"),
        path_var: std::env::var("PATH").expect("PATH"),
        sandbox: config.resolve(dirs.home(), None).expect("sandbox settings"),
        nono: Some(env_path("CR_TEST_NONO")),
        oauth_helper: None,
        dirs,
    };
    let session = SessionId::new(&ulid::Ulid::generate().to_string()).expect("session id");
    let request = AgentLaunch {
        session: session.clone(),
        cwd: cwd.clone(),
        agent_name: "pi".into(),
        agent: AgentEntry {
            command: agent_command(&pi, &mock),
            driver: DriverKind::Pi,
        },
        args: [
            "--mode",
            "rpc",
            "--no-themes",
            "--session-id",
            "0b9f1d2e-acce-4e00-8000-000000000001",
        ]
        .map(str::to_owned)
        .to_vec(),
        policy: effective_policy(
            [&ScopePolicy {
                commands: default_command_rules(),
                ..ScopePolicy::default()
            }],
            &ScopePolicy::default(),
        ),
        webhook_port: None,
    };

    let tmp_before = nono_dirs(nono_tmp.as_path());
    let prepared = launch::prepare(&host, &request).expect("prepare");
    let layout = prepared.sandbox.as_ref().expect("sandboxed").layout.clone();
    let info = launch::start(&host, &prepared).await.expect("start");
    let mut guard = Terminate(Some(info.socket.clone()));

    // The supervisor is detached: its launcher exited, it leads its own session, and it is
    // not our child.
    let sid = rustix::process::getsid(Some(
        rustix::process::Pid::from_raw(info.pid as i32).expect("pid"),
    ))
    .expect("getsid");
    assert_eq!(sid.as_raw_nonzero().get() as u32, info.pid);
    assert_ne!(info.pid, std::process::id());

    let client = Client::connect(&info.socket, true).await.expect("connect");
    let mut records = client.read(0).await.expect("read");
    let mut seen = Vec::new();

    client
        .write(r#"{"id":"state-1","type":"get_state"}"#)
        .await
        .expect("write get_state");
    let state = wait_out(&mut records, &mut seen, "the get_state response", |v| {
        v["id"] == "state-1" && v["type"] == "response"
    })
    .await;
    assert_eq!(state["success"], true, "{state}");
    let session_file = state["data"]["sessionFile"].as_str().unwrap_or_default();
    assert!(
        session_file.starts_with(layout.agent().as_str()),
        "{session_file}"
    );

    client
        .write(r#"{"id":"prompt-1","type":"prompt","message":"write a file, then try to escape"}"#)
        .await
        .expect("write prompt");
    let accepted = wait_out(&mut records, &mut seen, "the prompt response", |v| {
        v["id"] == "prompt-1" && v["type"] == "response"
    })
    .await;
    assert_eq!(accepted["success"], true, "{accepted}");
    wait_out(&mut records, &mut seen, "agent_settled", |v| {
        v["type"] == "agent_settled"
    })
    .await;

    assert_eq!(
        std::fs::read_to_string(cwd.join("inside.txt").as_path()).expect("inside.txt"),
        "written by pi\n"
    );
    assert!(
        !outside_file.as_path().exists(),
        "the sandbox let pi write outside cwd"
    );
    assert_eq!(mock.requests().len(), 3, "every scripted reply was used");
    let escape_result = mock.requests()[2].to_string();
    assert!(escape_result.contains("rc=1") || escape_result.contains("ermission denied"));

    // Terminate: the exit record arrives and the transcript holds every line.
    assert!(
        !client
            .terminate(Some(Duration::from_secs(5)))
            .await
            .expect("terminate")
    );
    guard.0 = None;
    let exit = wait_exit(&mut records, &mut seen).await;
    assert_eq!(exit, Some(StopCause::Terminate));

    let transcript = read_all(&info.transcript, 0)
        .expect("read transcript")
        .records;
    let ins: Vec<&str> = transcript
        .iter()
        .filter_map(|r| match &r.body {
            Body::In(line) => Some(line.raw.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ins.len(), 2, "{ins:?}");
    let outs = transcript
        .iter()
        .filter(|r| matches!(r.body, Body::Out(_)))
        .count();
    let seen_outs = seen
        .iter()
        .filter(|r| matches!(r.body, Body::Out(_)))
        .count();
    assert_eq!(outs, seen_outs);
    for record in &transcript {
        if let Body::Out(line) = &record.body {
            assert_eq!(line.truncated, 0);
            serde_json::from_str::<Value>(&line.raw)
                .unwrap_or_else(|e| panic!("non-JSON stdout line {:?}: {e}", line.raw));
        }
    }

    // nono cleaned up its temp dirs; anything left over is a leak.
    let leaked: Vec<String> = nono_dirs(nono_tmp.as_path())
        .into_iter()
        .filter(|d| !tmp_before.contains(d))
        .collect();
    clean_leaked_nono_dirs(nono_tmp.as_path(), Duration::ZERO);
    assert!(leaked.is_empty(), "leaked nono temp dirs: {leaked:?}");
}

/// Reads to the exit record and returns its stop cause.
async fn wait_exit(records: &mut RecordStream, seen: &mut Vec<Record>) -> Option<StopCause> {
    let exit = tokio::time::timeout(STEP_TIMEOUT, async {
        while let Some(record) = records.next().await {
            seen.push(record.clone());
            if let Body::Exit(exit) = &record.body {
                return Some(exit.cause);
            }
        }
        None
    })
    .await
    .expect("timed out waiting for the exit record");
    exit.expect("the stream ended without an exit record")
}
