//! Re-runs the nono spike's key checks against *generated* profiles on a real host.
//!
//! These tests need nono 0.80.0, Landlock, and a few binaries, so they are `#[ignore]`d.
//! Run them with `crates/sandbox/scripts/validate-nono.sh`, which builds the inputs with Nix
//! and sets:
//!
//! - `CR_TEST_NONO`: the nono binary;
//! - `CR_TEST_PATH`: a `PATH` with bash, coreutils, git, ssh, scp, curl and getent;
//! - `CR_TEST_SHADOW_PATH`: a directory with a *different* git store copy;
//! - `CR_TEST_CONTROLROOMD`: a built `controlroomd`;
//! - `CR_TEST_PI` (optional): a pi binary, for the RPC check.
//!
//! No test touches the network: model traffic goes to a local mock server, and every other
//! destination must be refused by the sandbox before leaving the host.

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use common::*;
use controlroom_sandbox::agent_dir::{AgentDirSpec, build_session_dirs, write_profile};
use controlroom_sandbox::launch::{LaunchCommand, Nono, NonoDirs};
use controlroom_sandbox::pins::resolve_pin;
use controlroom_sandbox::policy::{ScopePolicy, default_command_rules};
use controlroom_sandbox::probe::{self, Landlock, NonoStatus};
use controlroom_sandbox::spec::{Approvals, Endpoint, EndpointPolicy, SessionSandboxSpec};
use controlroom_sandbox::tempdirs::clean_leaked_nono_dirs;
use controlroom_sandbox::webhook::{
    ApprovalReply, ApprovalRequest, ApprovalSubject, HookSecret, HookUrl, parse_hook_path,
};
use controlroom_sandbox::{AbsPath, effective_policy, generate_profile};

const RUN_TIMEOUT: Duration = Duration::from_secs(180);

/// A request seen by the mock upstream: method, path, Authorization header.
type Seen = (String, String, Option<String>);

fn env_path(var: &str) -> AbsPath {
    let v = std::env::var(var).unwrap_or_else(|_| {
        panic!("{var} is not set; run crates/sandbox/scripts/validate-nono.sh")
    });
    AbsPath::new(&v).unwrap_or_else(|e| panic!("{var}: {e}"))
}

/// A scratch host layout: real-looking home, state dir, working dir, nono temp dir.
struct Host {
    _tmp: tempfile::TempDir,
    root: AbsPath,
    state: AbsPath,
    cwd: AbsPath,
    user_agent: AbsPath,
    nono: Nono,
    nono_tmp: AbsPath,
    path_var: String,
}

impl Host {
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("cr-nono-")
            // nono binds unix sockets under its TMPDIR: keep paths short (SUN_LEN).
            .tempdir_in("/tmp")
            .expect("tempdir");
        let root = AbsPath::from_path(tmp.path()).expect("utf-8 tempdir");
        let state = root.join("state");
        let cwd = root.join("work");
        let user_agent = root.join("home/.pi/agent");
        let nono_tmp = root.join("t");
        for d in [&cwd, &user_agent, &nono_tmp, &root.join("outside")] {
            std::fs::create_dir_all(d.as_path()).expect("mkdir");
        }
        std::fs::write(
            user_agent.join("auth.json").as_path(),
            serde_json::json!({"mock": {"type": "api_key", "key": "sk-test-key"}}).to_string(),
        )
        .expect("write auth.json");
        std::fs::write(root.join("outside/secret.txt").as_path(), "outside\n").expect("write");
        let path_var = format!(
            "{}:{}",
            std::env::var("CR_TEST_PATH").expect("CR_TEST_PATH"),
            env_path("CR_TEST_SHADOW_PATH")
        );
        Self {
            nono: Nono {
                binary: env_path("CR_TEST_NONO"),
                dirs: NonoDirs::under(&state),
            },
            _tmp: tmp,
            root,
            state,
            cwd,
            user_agent,
            nono_tmp,
            path_var,
        }
    }

    /// A strict spec for this host with the default mediated commands pinned from `PATH`
    /// and approvals going to `hook`.
    fn spec(&self, hook: &HookServer) -> SessionSandboxSpec {
        let mut spec = minimal(&self.state, &self.cwd);
        spec.approvals = Some(Approvals {
            hook: HookUrl {
                port: hook.port,
                session: spec.layout.id().clone(),
                secret: hook.secret.clone(),
            },
            timeout_secs: 30,
        });
        spec.protected.push(self.user_agent.clone());
        spec.credential_helper.controlroomd = env_path("CR_TEST_CONTROLROOMD");
        spec.policy = effective_policy(
            [&ScopePolicy {
                commands: default_command_rules(),
                ..ScopePolicy::default()
            }],
            &ScopePolicy::default(),
        );
        spec.pins = spec
            .policy
            .commands
            .iter()
            .filter_map(|r| {
                Some((
                    r.command.clone(),
                    resolve_pin(r.command.as_str(), &self.path_var)?,
                ))
            })
            .collect();
        spec
    }

    /// Builds the session dirs, writes and nono-validates the profile, and runs `script`
    /// under `bash -lc` in the sandbox.
    fn run(&self, spec: &SessionSandboxSpec, script: &str, stdin: &[u8]) -> Output {
        build_session_dirs(
            &spec.layout,
            &AgentDirSpec {
                user_agent_dir: Some(self.user_agent.clone()),
                ..AgentDirSpec::default()
            },
        )
        .expect("build session dirs");
        let profile = generate_profile(spec).expect("generate profile");
        write_profile(&spec.layout, &profile).expect("write profile");
        probe::validate_with_nono(&self.nono.binary, &self.nono.dirs, &spec.layout.profile())
            .expect("nono profile validate");

        let shell = resolve_pin("bash", &self.path_var).expect("bash on CR_TEST_PATH");
        let launch = LaunchCommand::new(
            Some((&self.nono, &spec.layout.profile())),
            &shell.executable,
            &spec.cwd,
            script,
        );
        let mut cmd = launch.to_command();
        cmd.env("PATH", &self.path_var)
            .env("HOME", self.root.join("home").as_str())
            .env("TMPDIR", self.nono_tmp.as_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = KillOnDrop(cmd.spawn().expect("spawn nono"));
        let mut input = child.0.stdin.take().expect("stdin");
        input.write_all(stdin).expect("write stdin");
        drop(input);
        let stdout = drain(child.0.stdout.take().expect("stdout"));
        let stderr = drain(child.0.stderr.take().expect("stderr"));
        let deadline = Instant::now() + RUN_TIMEOUT;
        let status = loop {
            if let Some(status) = child.0.try_wait().expect("wait") {
                break status;
            }
            assert!(Instant::now() < deadline, "sandboxed run timed out");
            std::thread::sleep(Duration::from_millis(20));
        };
        Output {
            status,
            stdout: stdout.join().expect("stdout reader"),
            stderr: stderr.join().expect("stderr reader"),
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Leave nothing behind in the shared /tmp either.
        clean_leaked_nono_dirs(self.nono_tmp.as_path(), Duration::ZERO);
    }
}

/// Kills the process (nono kills its sandboxed tree with it) even if the test panics.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn drain(mut r: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        buf
    })
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Lines `key=value` printed by a test script.
fn results(out: &Output) -> BTreeMap<String, String> {
    text(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// A minimal HTTP/1.1 server answering every request with `respond(method, path, body)`.
struct HttpServer {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

type Handler = dyn Fn(&str, &str, &BTreeMap<String, String>, &[u8]) -> (u16, Vec<u8>) + Send + Sync;

impl HttpServer {
    fn start(handler: Arc<Handler>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let handler = handler.clone();
                std::thread::spawn(move || serve(conn, &*handler));
            }
        });
        Self {
            port,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve(conn: TcpStream, handler: &Handler) {
    let mut reader = BufReader::new(conn.try_clone().expect("clone"));
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (
        parts.next().unwrap_or("").to_owned(),
        parts.next().unwrap_or("").to_owned(),
    );
    let mut headers = BTreeMap::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    let len = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    let _ = reader.read_exact(&mut body);
    let (status, reply) = handler(&method, &path, &headers, &body);
    let mut conn = conn;
    let _ = write!(
        conn,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reply.len()
    );
    let _ = conn.write_all(&reply);
}

/// The approval webhook: checks the URL secret, decodes the request with the crate's
/// types, records it, and decides with `decide`.
struct HookServer {
    port: u16,
    secret: HookSecret,
    seen: Arc<Mutex<Vec<ApprovalRequest>>>,
    _server: HttpServer,
}

impl HookServer {
    fn start(decide: fn(&ApprovalRequest) -> bool) -> Self {
        let secret = HookSecret::generate().expect("secret");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (secret2, seen2) = (secret.clone(), seen.clone());
        let server = HttpServer::start(Arc::new(
            move |_m: &str, path: &str, _h: &BTreeMap<String, String>, body: &[u8]| {
                let ok_path = parse_hook_path(path)
                    .is_some_and(|(s, k)| s.as_str() == SESSION && k.matches(&secret2));
                let reply = match ApprovalRequest::from_json(body) {
                    Ok(req) => {
                        let granted = ok_path && req.prompt().is_some() && decide(&req);
                        seen2.lock().expect("lock").push(req);
                        if granted {
                            ApprovalReply::granted()
                        } else {
                            ApprovalReply::denied("test policy")
                        }
                    }
                    Err(_) => ApprovalReply::denied("malformed"),
                };
                (200, reply.to_json())
            },
        ));
        Self {
            port: server.port,
            secret,
            seen,
            _server: server,
        }
    }

    fn seen(&self) -> Vec<ApprovalRequest> {
        self.seen.lock().expect("lock").clone()
    }
}

#[test]
#[ignore = "needs nono 0.80.0 and Landlock; run crates/sandbox/scripts/validate-nono.sh"]
fn host_probes() {
    assert!(
        matches!(probe::landlock(), Landlock::Abi(v) if v >= 4),
        "{:?}",
        probe::landlock()
    );
    let host = Host::new();
    assert_eq!(
        probe::nono(&host.nono.binary, &host.nono.dirs),
        NonoStatus::Available {
            version: "0.80.0".into(),
            pinned: true
        }
    );
}

#[test]
#[ignore = "needs nono 0.80.0 and Landlock; run crates/sandbox/scripts/validate-nono.sh"]
fn writes_are_confined_to_cwd_including_children_and_exit_codes_pass_through() {
    let host = Host::new();
    let hook = HookServer::start(|_| false);
    let spec = host.spec(&hook);
    let outside = host.root.join("outside");
    let script = format!(
        r#"
        echo in > ok.txt && echo cwd_write=ok
        (echo x > {o}/a.txt) 2>/dev/null && echo outside_write=BAD || echo outside_write=denied
        bash -c 'sh -c "echo x > {o}/b.txt"' 2>/dev/null && echo grandchild_write=BAD || echo grandchild_write=denied
        cat {o}/secret.txt >/dev/null 2>&1 && echo outside_read=BAD || echo outside_read=denied
        cat {agent}/auth.json >/dev/null 2>&1 && echo real_auth_read=BAD || echo real_auth_read=denied
        cat {profile} >/dev/null 2>&1 && echo profile_read=BAD || echo profile_read=denied
        echo x > "$HOME/h" && echo x > "$TMPDIR/t" && echo x > "$PI_CODING_AGENT_DIR/a" && echo session_dirs=ok
        # A detached grandchild stays confined. (With capability elevation, a process that
        # outlives nono fails closed: its file opens return ENOSYS.) Gate on it here.
        setsid nohup sh -c 'sleep 0.2; (echo x > {o}/late.txt) 2>/dev/null; echo done > late.done' >/dev/null 2>&1 &
        for _ in $(seq 300); do [ -e late.done ] && break; sleep 0.1; done
        [ -e late.done ] && echo detached=finished
        rm ok.txt && echo rm=ok
        exit 42
        "#,
        o = outside,
        agent = host.user_agent,
        profile = spec.layout.profile(),
    );
    let out = host.run(&spec, &script, b"");
    let r = results(&out);
    let expect = [
        ("cwd_write", "ok"),
        ("outside_write", "denied"),
        ("grandchild_write", "denied"),
        ("outside_read", "denied"),
        ("real_auth_read", "denied"),
        ("profile_read", "denied"),
        ("session_dirs", "ok"),
        ("detached", "finished"),
        ("rm", "ok"),
    ];
    for (k, v) in expect {
        assert_eq!(
            r.get(k).map(String::as_str),
            Some(v),
            "{k}; stderr: {}",
            text(&out.stderr)
        );
    }
    assert_eq!(out.status.code(), Some(42));

    assert!(!outside.join("late.txt").as_path().exists());
    assert!(!outside.join("a.txt").as_path().exists());
}

#[test]
#[ignore = "needs nono 0.80.0 and Landlock; run crates/sandbox/scripts/validate-nono.sh"]
fn network_allowlist_credential_injection_and_endpoint_approval() {
    let host = Host::new();
    let upstream_seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let seen2 = upstream_seen.clone();
    let upstream = HttpServer::start(Arc::new(
        move |m: &str, p: &str, h: &BTreeMap<String, String>, _b: &[u8]| {
            seen2.lock().expect("lock").push((
                m.to_owned(),
                p.to_owned(),
                h.get("authorization").cloned(),
            ));
            (200, br#"{"ok":true}"#.to_vec())
        },
    ));
    let hook = HookServer::start(
        |req| matches!(&req.subject, ApprovalSubject::Endpoint { path, .. } if path == "/v1/granted"),
    );
    let mut spec = host.spec(&hook);
    spec.credential_routes = vec![route(
        "mock",
        &format!("http://127.0.0.1:{}", upstream.port),
        EndpointPolicy {
            allow: vec![Endpoint::new("POST", "/v1/chat/completions")],
            approve: vec![
                Endpoint::new("GET", "/v1/granted"),
                Endpoint::new("GET", "/v1/refused"),
            ],
        },
    )];
    let script = format!(
        r#"
        c() {{ curl -s -o /dev/null -w '%{{http_code}}' -H "Authorization: Bearer $CR_MOCK_PHANTOM" "$@"; }}
        echo post=$(c -X POST -d '{{}}' "$CR_MOCK_BASE_URL/v1/chat/completions")
        echo unlisted=$(c "$CR_MOCK_BASE_URL/v1/models")
        echo approved=$(c "$CR_MOCK_BASE_URL/v1/granted")
        echo refused=$(c "$CR_MOCK_BASE_URL/v1/refused")
        c --noproxy '*' --max-time 5 http://127.0.0.1:{port}/v1/chat/completions >/dev/null; echo direct_rc=$?
        c --max-time 5 https://example.com/ >/dev/null; echo other_host_rc=$?
        (exec 3<>/dev/tcp/1.1.1.1/53) 2>/dev/null && echo raw_tcp=BAD || echo raw_tcp=denied
        getent hosts example.com >/dev/null 2>&1 && echo dns=BAD || echo dns=denied
        "#,
        port = upstream.port
    );
    let out = host.run(&spec, &script, b"");
    let r = results(&out);
    let get = |k: &str| {
        r.get(k).cloned().unwrap_or_else(|| {
            panic!(
                "{k} missing; stdout: {}; stderr: {}",
                text(&out.stdout),
                text(&out.stderr)
            )
        })
    };
    let dump = format!(
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert_eq!(get("post"), "200", "{dump}");
    assert_eq!(get("unlisted"), "403");
    assert_eq!(get("approved"), "200");
    assert_eq!(get("refused"), "403");
    assert_ne!(get("direct_rc"), "0");
    assert_ne!(get("other_host_rc"), "0");
    assert_eq!(get("raw_tcp"), "denied");
    assert_eq!(get("dns"), "denied");

    let seen = upstream_seen.lock().expect("lock").clone();
    let auth: Vec<_> = seen
        .iter()
        .map(|(m, p, a)| (m.as_str(), p.as_str(), a.as_deref()))
        .collect();
    assert_eq!(
        auth,
        [
            ("POST", "/v1/chat/completions", Some("Bearer sk-test-key")),
            ("GET", "/v1/granted", Some("Bearer sk-test-key")),
        ]
    );
    let endpoints: Vec<String> = hook
        .seen()
        .iter()
        .filter_map(|r| r.prompt())
        .map(|p| p.title)
        .collect();
    assert!(
        endpoints
            .iter()
            .any(|t| t.starts_with("Call GET /v1/granted")),
        "{endpoints:?}"
    );
    assert!(
        endpoints
            .iter()
            .any(|t| t.starts_with("Call GET /v1/refused")),
        "{endpoints:?}"
    );
}

#[test]
#[ignore = "needs nono 0.80.0 and Landlock; run crates/sandbox/scripts/validate-nono.sh"]
fn mediated_commands_hit_the_policy_through_path_and_absolute_paths() {
    let host = Host::new();
    let hook = HookServer::start(|req| match &req.subject {
        ApprovalSubject::Command { args, .. } => args.iter().any(|a| a.ends_with(":granted")),
        _ => false,
    });
    let spec = host.spec(&hook);
    let git = &spec.pins[&name("git")];
    assert!(
        !git.shadows.is_empty(),
        "CR_TEST_SHADOW_PATH must hold a different git"
    );
    let script = format!(
        r#"
        git init -q --bare remote.git && git init -q repo && cd repo
        git -c user.email=alice@example.com -c user.name=alice commit -q --allow-empty -m init && echo commit=ok
        git remote add origin ../remote.git
        git push -q origin HEAD:granted 2>/dev/null; echo push_granted=$?
        git push -q origin HEAD:denied 2>/dev/null; echo push_denied=$?
        {git} push -q origin HEAD:abs 2>/dev/null; echo push_abs=$?
        {shadow} --version >/dev/null 2>&1; echo shadow=$?
        ssh -V 2>/dev/null; echo ssh=$?
        {ssh} -V 2>/dev/null; echo ssh_abs=$?
        scp 2>/dev/null; echo scp=$?
        git -c core.sshCommand=ssh ls-remote ssh://127.0.0.1:1/x 2>&1 | grep -q 'command policy denied' && echo git_ssh=denied || echo git_ssh=allowed
        touch a && cp a b && mv b c && rm a c && echo coreutils=ok
        "#,
        git = git.executable,
        shadow = git.shadows[0],
        ssh = spec.pins[&name("ssh")].executable,
    );
    let out = host.run(&spec, &script, b"");
    let r = results(&out);
    let get = |k: &str| {
        r.get(k).cloned().unwrap_or_else(|| {
            panic!(
                "{k} missing; stdout: {}; stderr: {}",
                text(&out.stdout),
                text(&out.stderr)
            )
        })
    };
    assert_eq!(get("commit"), "ok");
    assert_eq!(get("push_granted"), "0");
    assert_eq!(get("push_denied"), "126");
    // A direct call of the pinned file is not a bypass: nono denies it outright.
    assert_eq!(get("push_abs"), "126");
    assert_eq!(get("shadow"), "126");
    assert_eq!(get("ssh"), "126");
    assert_eq!(get("ssh_abs"), "126");
    assert_eq!(get("scp"), "126");
    assert_eq!(get("git_ssh"), "allowed");
    assert_eq!(get("coreutils"), "ok");

    let commands: Vec<Vec<String>> = hook
        .seen()
        .into_iter()
        .filter_map(|r| match r.subject {
            ApprovalSubject::Command { args, .. } => Some(args),
            _ => None,
        })
        .collect();
    let pushes: Vec<&str> = commands
        .iter()
        .filter_map(|a| {
            a.iter()
                .find(|x| x.starts_with("HEAD:"))
                .map(String::as_str)
        })
        .collect();
    assert_eq!(pushes, ["HEAD:granted", "HEAD:denied"], "{commands:?}");
}

#[test]
#[ignore = "needs nono 0.80.0, Landlock and CR_TEST_PI; run crates/sandbox/scripts/validate-nono.sh"]
fn pi_rpc_stdout_is_clean_jsonl() {
    let Ok(pi) = std::env::var("CR_TEST_PI") else {
        panic!("CR_TEST_PI is not set");
    };
    let mut host = Host::new();
    let pi = AbsPath::new(&pi).expect("absolute CR_TEST_PI");
    host.path_var = format!("{}:{}", host.path_var, pi.parent().expect("pi dir"));
    let hook = HookServer::start(|_| false);
    let spec = host.spec(&hook);
    let requests = b"{\"id\":\"1\",\"type\":\"get_state\"}\n";
    let out = host.run(&spec, "exec pi --mode rpc --no-themes", requests);
    let stdout = text(&out.stdout);
    assert!(
        !stdout.is_empty(),
        "no output; stderr: {}",
        text(&out.stderr)
    );
    let mut responded = false;
    for line in stdout.lines() {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("non-JSON stdout line {line:?}: {e}"));
        if v["id"] == "1" && v["type"] == "response" {
            assert_eq!(v["success"], true, "{line}");
            let file = v["data"]["sessionFile"].as_str().unwrap_or_default();
            assert!(file.starts_with(spec.layout.agent().as_str()), "{file}");
            responded = true;
        }
    }
    assert!(responded, "no get_state response: {stdout}");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
}
