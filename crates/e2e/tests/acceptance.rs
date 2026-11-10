//! Acceptance of the remote-tools model on a dev machine: the e2e topology (controller, the
//! real agent daemon, two real host daemons over SSH with cuttable links) with **real pi 1.2.0
//! and the `controlroom-pi` bundle** (the real `@controlroom/pi` remote tools) **under nono at
//! level strict on the server**, and **nono-confined workspaces on the hosts**, driven by the
//! fixture tool's mock model:
//!
//! 1. a read on devbox, an edit on buildbox (pi's edit over `read_bytes` + a compare-and-swap
//!    write), and a named shell that keeps its state (`shell_new`, `bash {shell}`); a write
//!    outside the workspace fails inside the host's sandbox;
//! 2. a link blip while a `bash` runs: the result arrives once, with the link-drop note;
//! 3. a background job whose exit wakes the agent once.
//!
//! A second test covers the sandbox's file requests: harmless runtime probes raise none, and
//! "Always allow" and "Never" carry over to new sessions on the machine.
//!
//! Needs nono 0.80.0, Landlock and pi, so it is `#[ignore]`d; run it with
//! `crates/e2e/scripts/acceptance-pi-nono.sh`. Every process it starts is killed at the end,
//! pass or fail.

use std::path::{Path, PathBuf};
use std::time::Duration;

use controlroom_e2e::held::Held;
use controlroom_e2e::world::{Host, Which, World};
use controlroom_fixture_tool::mock_model::{MockModel, Reply};
use serde_json::{Value, json};

fn env_var(var: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| {
        panic!("{var} is not set; run crates/e2e/scripts/acceptance-pi-nono.sh")
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
fn configure_server(agentd: &controlroom_e2e::agentd::Agentd, mock: &MockModel, nono_tmp: &Path) {
    let agent_dir = agentd.state().join("agent");
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
    let bundle = PathBuf::from(env_var("CR_TEST_PI_BUNDLE"));
    agentd.agent(&agent_command(&bundle, mock));
    std::fs::write(
        agentd.config().join("sandbox.toml"),
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
    // The login shell runs inside pi's sandbox, so it must be readable there.
    agentd.login_shell(Path::new("/bin/sh"));
}

/// A host whose workspaces run under nono, with a real login shell for jobs.
fn configure_host(host: &Host, nono_tmp: &Path) {
    let config = host.dir(".config/controlroom");
    std::fs::write(
        config.join("sandbox.toml"),
        format!(
            "nono = {:?}\nnono_tmp = {nono_tmp:?}\n",
            env_var("CR_TEST_NONO")
        ),
    )
    .unwrap();
    host.set_env("SHELL", &env_var("CR_TEST_BASH"));
}

/// Short temp dirs for nono (it binds sockets there); removes what a killed nono left.
struct NonoTmp(Vec<tempfile::TempDir>);

impl NonoTmp {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn dir(&mut self) -> PathBuf {
        let dir = tempfile::Builder::new()
            .prefix("crn")
            .tempdir_in("/tmp")
            .unwrap();
        let path = dir.path().to_owned();
        self.0.push(dir);
        path
    }
}

impl Drop for NonoTmp {
    fn drop(&mut self) {
        for dir in &self.0 {
            controlroom_sandbox::tempdirs::clean_leaked_nono_dirs(dir.path(), Duration::ZERO);
        }
    }
}

/// Every request's messages as one string, from request `from` on.
fn seen(mock: &MockModel, from: usize) -> String {
    mock.requests()[from..]
        .iter()
        .map(|r| r["messages"].to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs nono 0.80.0, Landlock and pi; run crates/e2e/scripts/acceptance-pi-nono.sh"]
async fn real_pi_under_nono_works_on_two_nono_confined_hosts() {
    let mock = MockModel::start(Vec::new()).await.expect("mock model");
    // Declared before the world, so it is dropped (and cleaned) after the world killed nono.
    let mut nono_tmp = NonoTmp::new();
    let (server_tmp, devbox_tmp, buildbox_tmp) = (nono_tmp.dir(), nono_tmp.dir(), nono_tmp.dir());
    let mut world = World::start_with(|agentd, devbox, buildbox| {
        configure_server(agentd, &mock, &server_tmp);
        configure_host(devbox, &devbox_tmp);
        configure_host(buildbox, &buildbox_tmp);
    })
    .await;
    let api = world.devbox.dir("src/api");
    std::fs::create_dir_all(api.join("sub")).unwrap();
    std::fs::write(api.join("notes.txt"), "remote notes\n").unwrap();
    std::fs::write(api.join("AGENTS.md"), "Use the builder shell.\n").unwrap();
    let web = world.buildbox.dir("work/web");
    std::fs::write(web.join("app.js"), "const version = 'v1';\n").unwrap();

    let (devbox, buildbox) = (world.devbox.id.clone(), world.buildbox.id.clone());
    let params = world.create_params(
        &[(&devbox, "~/src/api"), (&buildbox, "~/work/web")],
        json!({}),
    );
    let mut params = params;
    params.as_object_mut().unwrap().remove("sandbox_level");
    let created = world.ui.ok("session.create", params).await;
    let id = created["session"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["session"]["sandbox"]["level"], "strict");
    world
        .wait_session(&id, "idle", |s| {
            s["state"] == "idle" && s["agent_live"] == true
        })
        .await;

    // 1. Files on both machines and a named shell.
    mock.push([
        Reply::tool_call("call_read", "read", json!({"path": "devbox:notes.txt"})),
        Reply::tool_call(
            "call_edit",
            "edit",
            json!({"path": "buildbox:app.js",
                   "edits": [{"oldText": "'v1'", "newText": "'v2'"}]}),
        ),
        Reply::tool_call(
            "call_new",
            "shell_new",
            json!({"name": "builder", "at": "devbox:~/src/api"}),
        ),
        Reply::tool_call(
            "call_env",
            "bash",
            json!({"shell": "builder",
                   "command": "cd sub && export STAGE=ready; (echo x > ~/planted.txt) 2>/dev/null || echo confined"}),
        ),
        Reply::tool_call(
            "call_state",
            "bash",
            json!({"shell": "builder", "command": "pwd; echo \"[$STAGE]\""}),
        ),
        Reply::text("Files and shells done."),
    ]);
    world.prompt(&id, "Read, edit and set up a shell.").await;
    let all = seen(&mock, 0);
    assert!(all.contains("remote notes"), "the read: {all}");
    assert!(
        all.contains("Use the builder shell."),
        "devbox's AGENTS.md: {all}"
    );
    assert_eq!(
        std::fs::read_to_string(web.join("app.js")).unwrap(),
        "const version = 'v2';\n"
    );
    assert!(
        all.contains("confined"),
        "the write outside the workspace: {all}"
    );
    assert!(!world.devbox.home().join("planted.txt").exists());
    let sub = api.join("sub");
    assert!(
        all.contains(&format!("{}\\n[ready]", sub.display())),
        "the shell kept its state: {all}"
    );
    assert!(all.contains("[shell builder"), "{all}");
    // The hosts' shells run under nono.
    for host in [&world.devbox, &world.buildbox] {
        assert!(
            !host.processes("nono run").is_empty(),
            "{}: no sandboxed shell",
            host.name
        );
    }

    // The server side: pi ran under nono, in an empty scratch directory, without local tools.
    let transcript = world
        .agentd
        .state()
        .join(format!("sessions/{id}/transcript.jsonl"));
    let records = controlroom_transcript::read_all(&transcript, 0)
        .unwrap()
        .records;
    let controlroom_transcript::Body::Meta(meta) = &records[0].body else {
        panic!("the transcript starts with its meta record");
    };
    assert!(meta.argv[0].ends_with("/nono"), "{:?}", meta.argv);
    let launched = meta.argv.join(" ");
    for flag in ["--no-builtin-tools", "--no-context-files", "--no-skills"] {
        assert!(launched.contains(flag), "{launched}");
    }
    let workdir = world.agentd.state().join(format!("sessions/{id}/work"));
    assert_eq!(std::fs::read_dir(&workdir).unwrap().count(), 0);

    // 2. A link blip while a command runs: the result arrives once, with a note.
    let held = Held::new(&api, "slow");
    let from = mock.requests().len();
    mock.push([
        Reply::tool_call(
            "call_slow",
            "bash",
            json!({"shell": "builder", "command": held.command("echo slow-start", "echo slow-end")}),
        ),
        Reply::text("The slow step finished."),
    ]);
    let mark = world.turn_mark(&id);
    world.send_prompt(&id, "Run the slow step.").await;
    held.started().await;
    world.cut(Which::Devbox).await;
    world.wait_availability(&id, &devbox, false).await;
    held.release().await;
    world.restore(Which::Devbox).await;
    world.wait_turn(&id, mark).await;
    let after = seen(&mock, from);
    assert!(after.contains("slow-start\\nslow-end"), "{after}");
    assert!(
        after.contains("The link to devbox dropped once during this call"),
        "{after}"
    );
    assert_eq!(held.runs(), 1);

    // 3. A background job: its exit wakes the agent once.
    let from = mock.requests().len();
    mock.push([
        Reply::tool_call(
            "call_job",
            "job",
            json!({"action": "run", "command": "echo building; echo built", "name": "build",
                   "at": "devbox:~/src/api"}),
        ),
        Reply::text("The build runs."),
        Reply::text("The build is done."),
    ]);
    world.prompt(&id, "Start the build.").await;
    let mut timeline = world.timeline(&id).await;
    world
        .until(&mut timeline, "the exit wake", |t| {
            t.user_texts()
                .iter()
                .any(|m| m.contains("`build` exited with code 0"))
        })
        .await;
    let mark = world.turn_mark(&id);
    let summary = world.session(&id);
    if summary["state"] != "idle" {
        world.wait_turn(&id, mark).await;
    }
    let wakes: Vec<String> = timeline
        .user_texts()
        .into_iter()
        .filter(|m| m.contains("Background job update"))
        .collect();
    assert_eq!(wakes.len(), 1, "{wakes:#?}");
    let after = seen(&mock, from);
    assert!(
        after.contains("exited with code 0"),
        "the model saw the wake"
    );

    world
        .ui
        .ok(
            "session.close",
            json!({"op_id": controlroom_controller::test_support::op(), "id": id}),
        )
        .await;
    world
        .wait_session(&id, "closed", |s: &Value| s["state"] == "closed")
        .await;
}

/// Answers the session's pending sandbox file requests until one for a path ending in `target`
/// comes: that one gets `option`, the others are denied. Returns the request.
async fn answer_file_request(world: &mut World, id: &str, target: &str, option: &str) -> Value {
    loop {
        let summary = world
            .wait_session(id, "a pending file request", |s| {
                s["requests"]
                    .as_array()
                    .is_some_and(|r| r.iter().any(|r| r["body"]["kind"] == "path"))
            })
            .await;
        let request = summary["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["body"]["kind"] == "path")
            .unwrap()
            .clone();
        let ours = request["body"]["path"]
            .as_str()
            .is_some_and(|p| p.ends_with(target));
        let chosen = if ours { option } else { "deny" };
        world
            .ui
            .ok(
                "request.respond",
                json!({"op_id": controlroom_controller::test_support::op(), "id": id,
                       "request_id": request["id"],
                       "response": {"kind": "choice", "option_id": chosen}}),
            )
            .await;
        if ours {
            return request;
        }
    }
}

/// The file requests a session's timeline shows.
async fn file_requests(world: &mut World, id: &str) -> Vec<Value> {
    let timeline = world.timeline(id).await;
    let found = timeline
        .of_kind("request")
        .into_iter()
        .filter(|r| r["request"]["body"]["kind"] == "path")
        .cloned()
        .collect();
    world.unsubscribe(timeline).await;
    found
}

/// Spec §11 "Harmless defaults" and the standing decisions on file requests, end to end with
/// real pi and nono: a strict session reads the runtime probes without a request; "Always
/// allow" and "Never" on its file requests carry over to a new session on the machine, which
/// reads the allowed path and is refused the never-path without any request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs nono 0.80.0, Landlock and pi; run crates/e2e/scripts/acceptance-pi-nono.sh"]
async fn always_and_never_on_file_requests_carry_over_to_new_sessions() {
    let mock = MockModel::start(Vec::new()).await.expect("mock model");
    let mut nono_tmp = NonoTmp::new();
    let (server_tmp, devbox_tmp, buildbox_tmp) = (nono_tmp.dir(), nono_tmp.dir(), nono_tmp.dir());
    let mut world = World::start_with(|agentd, devbox, buildbox| {
        configure_server(agentd, &mock, &server_tmp);
        configure_host(devbox, &devbox_tmp);
        configure_host(buildbox, &buildbox_tmp);
    })
    .await;
    world.devbox.dir("src/api");
    let notes = world.devbox.dir("notes");
    std::fs::write(notes.join("allowed.txt"), "allowed-content\n").unwrap();
    std::fs::write(notes.join("never.txt"), "never-content\n").unwrap();
    let devbox = world.devbox.id.clone();
    let strict_session = |world: &World| {
        let mut params = world.create_params(&[(&devbox, "~/src/api")], json!({}));
        params.as_object_mut().unwrap().remove("sandbox_level");
        params
    };
    let created = world.ui.ok("session.create", strict_session(&world)).await;
    let first = created["session"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["session"]["sandbox"]["level"], "strict");
    world
        .wait_session(&first, "idle", |s| {
            s["state"] == "idle" && s["agent_live"] == true
        })
        .await;
    let bash =
        |call: &str, command: &str| Reply::tool_call(call, "bash", json!({"command": command}));

    // 1. The runtime probes: no request.
    mock.push([
        bash(
            "call_probe",
            "cat /proc/sys/vm/nr_hugepages >/dev/null && echo probed",
        ),
        Reply::text("Probed."),
    ]);
    world.prompt(&first, "Probe the huge pages.").await;
    assert!(seen(&mock, 0).contains("probed"), "{}", seen(&mock, 0));

    // 2. "Always allow" on one path, "Never" on another.
    let from = mock.requests().len();
    mock.push([
        bash("call_allowed", "cat ~/notes/allowed.txt"),
        bash("call_never", "cat ~/notes/never.txt 2>&1"),
        Reply::text("Read them."),
    ]);
    let mark = world.turn_mark(&first);
    world.send_prompt(&first, "Read the notes.").await;
    let always =
        answer_file_request(&mut world, &first, "/notes/allowed.txt", "allow-always").await;
    assert_eq!(always["body"]["machine"], "devbox");
    answer_file_request(&mut world, &first, "/notes/never.txt", "never").await;
    world.wait_turn(&first, mark).await;
    let after = seen(&mock, from);
    assert!(after.contains("allowed-content"), "{after}");
    assert!(!after.contains("never-content"), "{after}");
    let probes = file_requests(&mut world, &first).await;
    assert!(
        !probes.iter().any(|r| r["request"]["body"]["path"]
            .as_str()
            .is_some_and(|p| p.starts_with("/proc/sys"))),
        "{probes:#?}"
    );
    let profiles = world.ui.ok("sandbox.profiles", json!({})).await;
    let profile = &profiles["profiles"][0];
    assert_eq!(profile["scope"]["machine_id"], json!(devbox));
    assert!(
        profile["grants"][0]["value"]
            .as_str()
            .unwrap()
            .ends_with("/notes/allowed.txt")
    );
    assert!(
        profile["never"][0]
            .as_str()
            .unwrap()
            .ends_with("/notes/never.txt")
    );

    // 3. A new session: the allowed path reads, the never-path is refused, nothing is asked.
    let created = world.ui.ok("session.create", strict_session(&world)).await;
    let second = created["session"]["id"].as_str().unwrap().to_owned();
    world
        .wait_session(&second, "idle", |s| {
            s["state"] == "idle" && s["agent_live"] == true
        })
        .await;
    let from = mock.requests().len();
    mock.push([
        bash(
            "call_again",
            "cat ~/notes/allowed.txt; cat ~/notes/never.txt 2>&1 || echo refused",
        ),
        Reply::text("Done."),
    ]);
    world.prompt(&second, "Read the notes again.").await;
    let after = seen(&mock, from);
    assert!(after.contains("allowed-content"), "{after}");
    assert!(
        after.contains("refused") && !after.contains("never-content"),
        "{after}"
    );
    assert!(file_requests(&mut world, &second).await.is_empty());

    for id in [first, second] {
        world
            .ui
            .ok(
                "session.close",
                json!({"op_id": controlroom_controller::test_support::op(), "id": id}),
            )
            .await;
    }
}
