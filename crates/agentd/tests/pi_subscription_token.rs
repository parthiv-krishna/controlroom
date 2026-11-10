//! The real pi request path of an Anthropic subscription token pasted in Settings › Models: the
//! launch merge and the sandbox crate build the session's agent dir, and pi 1.2.0 (the
//! `controlroom-pi` bundle) runs one prompt on it against a local mock of Anthropic's Messages
//! API (the session's `models.json` points `anthropic` there, as the VM test's `pi-mock` does
//! for its mock). pi must take its subscription path with the stand-in alone: `Authorization:
//! Bearer sk-ant-oat01-controlroom-placeholder` (which nono's proxy replaces with the real token)
//! and Claude Code's `anthropic-beta`, never `x-api-key`.
//!
//! Needs the bundle, so it is `#[ignore]`d; the flake check `agentd-pi-subscription-token` runs
//! it with `CR_TEST_PI_BUNDLE` set (or: `nix build -o /tmp/pi .#controlroom-pi` and
//! `CR_TEST_PI_BUNDLE=/tmp/pi cargo test -p controlroom-agentd --test pi_subscription_token --
//! --ignored`). pi is killed at the end, pass or fail.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use controlroom_agentd::config::sandbox as sandbox_config;
use controlroom_agentd::dirs::Dirs;
use controlroom_agentd::models::merge;
use controlroom_sandbox::agent_dir::{PLACEHOLDER, build_session_dirs};
use controlroom_sandbox::credential::KeyStore;
use controlroom_sandbox::{AbsPath, SessionId, SessionLayout};
use serde_json::json;

const SESSION: &str = "01HZY3K4M5N6P7Q8R9S0T1V2W3";
const TOKEN: &str = "sk-ant-oat01-test-secret-abcd";

/// How long pi may take for one prompt.
const PI_TIMEOUT: Duration = Duration::from_secs(120);

/// One request as the mock received it: the request line and lowercased headers.
struct Received {
    request_line: String,
    headers: Vec<(String, String)>,
}

impl Received {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A mock of Anthropic's Messages API on loopback: every request gets a streamed "ok"; each is
/// sent to the returned channel. Its thread ends with the test process.
fn mock_anthropic() -> (String, mpsc::Receiver<Received>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let origin = format!("http://{}", listener.local_addr().expect("addr"));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            if let Some(received) = answer(stream)
                && tx.send(received).is_err()
            {
                return;
            }
        }
    });
    (origin, rx)
}

/// Reads one request and answers it with a minimal Messages API stream.
fn answer(mut stream: TcpStream) -> Option<Received> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.push((name.trim().to_lowercase(), value.trim().to_owned()));
    }
    let received = Received {
        request_line: request_line.trim_end().to_owned(),
        headers,
    };
    let length = received
        .header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    let events = [
        (
            "message_start",
            json!({"type": "message_start", "message": {"id": "msg_1", "type": "message",
                "role": "assistant", "model": "mock-model", "content": [], "stop_reason": null,
                "usage": {"input_tokens": 1, "output_tokens": 0}}}),
        ),
        (
            "content_block_start",
            json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}}),
        ),
        (
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": "ok"}}),
        ),
        (
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0}),
        ),
        (
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
                "usage": {"output_tokens": 1}}),
        ),
        ("message_stop", json!({"type": "message_stop"})),
    ];
    let mut response = String::from(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
    );
    for (event, data) in events {
        response.push_str(&format!("event: {event}\ndata: {data}\n\n"));
    }
    stream.write_all(response.as_bytes()).ok()?;
    Some(received)
}

/// Kills the child when dropped.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The session's agent dir as a launch builds it with `TOKEN` stored for Anthropic.
fn session_agent_dir(root: &AbsPath) -> AbsPath {
    let dirs = Dirs::under(root.join("state"));
    std::fs::create_dir_all(dirs.models().as_path()).expect("mkdir");
    let mut keys = KeyStore::default();
    keys.insert("anthropic", TOKEN);
    keys.save(&dirs.models().join("keys.json"))
        .expect("save keys");
    let mut sandbox = sandbox_config::SandboxConfig::default()
        .resolve(None, &dirs.pi_agent_dir())
        .expect("sandbox");
    merge::apply(&dirs, &mut sandbox);
    let layout = SessionLayout::new(dirs.state(), &SessionId::new(SESSION).expect("id"));
    build_session_dirs(&layout, &sandbox.agent_dir).expect("build session dirs");
    layout.agent()
}

/// Points the session's `anthropic` at `origin`, with one mock model to pick.
fn point_anthropic_at(agent: &AbsPath, origin: &str) {
    let path = agent.join("models.json");
    assert!(
        !path.as_path().exists(),
        "no other models.json entries here"
    );
    let models = json!({"providers": {"anthropic": {
        "baseUrl": origin,
        "models": [{"id": "mock-model", "name": "Mock", "input": ["text"],
                    "contextWindow": 100000, "maxTokens": 1000,
                    "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}}]
    }}});
    std::fs::write(path.as_path(), models.to_string()).expect("write models.json");
}

/// Runs one prompt with the bundle's launcher on `agent`, in an isolated home.
fn run_pi(bundle: &Path, agent: &AbsPath, root: &Path) -> std::process::Output {
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");
    let mut command = Command::new(bundle.join("bin/controlroom-pi"));
    command
        .args([
            "--",
            "-p",
            "hi",
            "--provider",
            "anthropic",
            "--model",
            "mock-model",
        ])
        .arg("--no-session")
        .current_dir(&home)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("PI_CODING_AGENT_DIR", agent.as_str())
        .env("PI_OFFLINE", "1")
        .env("PI_TELEMETRY", "0")
        .env("PI_SKIP_VERSION_CHECK", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = KillOnDrop(command.spawn().expect("run controlroom-pi"));
    let (tx, rx) = mpsc::channel();
    let mut stdout = child.0.stdout.take().expect("stdout");
    let mut stderr = child.0.stderr.take().expect("stderr");
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        let _ = stderr.read_to_end(&mut err);
        let _ = tx.send((out, err));
    });
    let (stdout, stderr) = rx.recv_timeout(PI_TIMEOUT).expect("pi finishes the prompt");
    let status = child.0.wait().expect("wait for pi");
    std::process::Output {
        status,
        stdout,
        stderr,
    }
}

#[test]
#[ignore = "needs the controlroom-pi bundle (CR_TEST_PI_BUNDLE); run by the flake check agentd-pi-subscription-token"]
fn pi_sends_the_stand_in_as_a_subscription_token() {
    let bundle = std::env::var_os("CR_TEST_PI_BUNDLE")
        .expect("CR_TEST_PI_BUNDLE is the controlroom-pi bundle");
    let t = tempfile::tempdir().expect("tempdir");
    let root = AbsPath::from_path(t.path()).expect("utf-8 tempdir");
    let agent = session_agent_dir(&root);
    let stand_in = format!("sk-ant-oat01-{PLACEHOLDER}");
    let auth = std::fs::read_to_string(agent.join("auth.json").as_path()).expect("auth.json");
    assert!(auth.contains(&stand_in) && !auth.contains(TOKEN), "{auth}");

    let (origin, requests) = mock_anthropic();
    point_anthropic_at(&agent, &origin);
    let out = run_pi(Path::new(&bundle), &agent, t.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "pi failed: {stderr}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "ok",
        "{stderr}"
    );

    let request = requests.try_recv().expect("pi called the mock");
    assert!(
        request.request_line.starts_with("POST /v1/messages"),
        "{}",
        request.request_line
    );
    assert_eq!(
        request.header("authorization"),
        Some(format!("Bearer {stand_in}").as_str())
    );
    assert_eq!(request.header("x-api-key"), None);
    let beta = request.header("anthropic-beta").unwrap_or_default();
    assert!(
        beta.split(',').any(|f| f.trim() == "oauth-2025-04-20"),
        "{beta}"
    );
    assert!(
        request
            .header("user-agent")
            .is_some_and(|ua| ua.starts_with("claude-cli/")),
        "{:?}",
        request.header("user-agent")
    );
    assert_eq!(request.header("x-app"), Some("cli"));
}
