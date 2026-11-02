use controlroom_protocol::session::DriverKind;
use controlroom_sandbox::policy::{CommandRule, ScopePolicy, default_command_rules};
use controlroom_sandbox::{Name, effective_policy};
use serde_json::Value;

use super::*;
use crate::config::sandbox::SandboxConfig;

const SESSION: &str = "01HZY3K4M5N6P7Q8R9S0T1V2W3";

struct Fixture {
    _tmp: tempfile::TempDir,
    root: AbsPath,
    host: Host,
}

fn fixture(sandbox_toml: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = AbsPath::from_path(tmp.path()).unwrap();
    let home = root.join("home");
    let user_agent = home.join(".pi/agent");
    std::fs::create_dir_all(user_agent.as_path()).unwrap();
    std::fs::create_dir_all(root.join("work").as_path()).unwrap();
    std::fs::write(
        user_agent.join("models.json").as_path(),
        serde_json::json!({"providers": {"mock": {
            "baseUrl": "http://127.0.0.1:4000/v1", "api": "openai-completions",
            "apiKey": "sk-test-key", "models": [{"id": "m"}]
        }}})
        .to_string(),
    )
    .unwrap();
    let config: SandboxConfig = toml::from_str(sandbox_toml).unwrap();
    let host = Host {
        dirs: Dirs::under_home(home.clone()),
        controlroomd: AbsPath::new("/opt/controlroom/versions/0.1.0/controlroomd").unwrap(),
        shell: AbsPath::new("/bin/sh").unwrap(),
        path_var: String::new(),
        sandbox: config.resolve(&home, None).unwrap(),
        nono: Some(AbsPath::new("/nix/store/aaaa-nono-0.80.0/bin/nono").unwrap()),
    };
    Fixture {
        _tmp: tmp,
        root,
        host,
    }
}

fn launch(f: &Fixture, level: Option<SandboxLevel>, webhook_port: Option<u16>) -> AgentLaunch {
    let machine = ScopePolicy {
        commands: default_command_rules(),
        ..ScopePolicy::default()
    };
    AgentLaunch {
        session: SessionId::new(SESSION).unwrap(),
        cwd: f.root.join("work"),
        agent_name: "pi".into(),
        agent: AgentEntry {
            command: "pi".into(),
            driver: DriverKind::Pi,
        },
        args: vec![
            "--mode".into(),
            "rpc".into(),
            "--session-id".into(),
            "a b".into(),
        ],
        policy: effective_policy(
            [&machine],
            &ScopePolicy {
                level,
                ..ScopePolicy::default()
            },
        ),
        webhook_port,
    }
}

fn argv(prepared: &PreparedLaunch) -> &[String] {
    match &prepared.request.spec.exec {
        Exec::Argv(argv) => argv,
        Exec::Shell(_) => panic!("expected an argv"),
    }
}

const LOOPBACK_ROUTE: &str = r#"
allow_loopback_upstreams = true
[[routes]]
provider = "mock"
upstream = "http://127.0.0.1:4000"
allow = ["POST /v1/chat/completions"]
[[routes]]
provider = "unused"
upstream = "https://llm.example.com"
allow = ["POST /v1/chat/completions"]
"#;

#[test]
fn strict_launch_runs_nono_with_a_written_profile() {
    let f = fixture(LOOPBACK_ROUTE);
    let prepared = prepare(&f.host, &launch(&f, None, None)).unwrap();
    let state = f.host.dirs.state();
    let session_dir = state.join("sessions").join(SESSION);

    let argv = argv(&prepared);
    assert_eq!(argv[0], "/nix/store/aaaa-nono-0.80.0/bin/nono");
    let tail = &argv[argv.len() - 3..];
    assert_eq!(tail[0], "/bin/sh");
    assert_eq!(tail[1], "-lc");
    assert_eq!(tail[2], "exec pi --mode rpc --session-id 'a b'");

    let request = &prepared.request;
    assert_eq!(request.id, SESSION);
    assert_eq!(request.paths.dir, session_dir.as_path());
    assert_eq!(
        request.paths.socket,
        state.join(&format!("run/{SESSION}.sock")).as_path()
    );
    request.paths.validate().unwrap();
    request.spec.validate().unwrap();
    assert_eq!(request.spec.labels["sandbox"], "strict");

    let env = std::fs::read_to_string(request.spec.env_file.as_ref().unwrap()).unwrap();
    let env = controlroom_supervisor::parse_env(&env).unwrap();
    let get = |k: &str| {
        env.iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(get("TMPDIR"), Some("/tmp"));
    assert_eq!(get("NONO_NO_UPDATE_CHECK"), Some("1"));
    assert_eq!(
        get("XDG_STATE_HOME"),
        Some(state.join("nono/state").as_str())
    );

    let profile: Value = serde_json::from_str(
        &std::fs::read_to_string(session_dir.join("profile.json").as_path()).unwrap(),
    )
    .unwrap();
    // Only providers with a key get a route; the helper is this controlroomd.
    let routes = profile["network"]["custom_credentials"]
        .as_object()
        .unwrap();
    assert_eq!(routes.keys().collect::<Vec<_>>(), ["cr_mock"]);
    let helper = &profile["credential_capture"]["cr_mock"]["command"];
    assert_eq!(helper[0], "/opt/controlroom/versions/0.1.0/controlroomd");
    assert_eq!(helper[3], state.as_str());
    // No webhook: no elevation, and nothing waits for an approval.
    assert!(profile.get("security").is_none());
    let text = profile.to_string();
    assert!(!text.contains("approve"), "{text}");
    assert!(!text.contains("daemon.sock"), "{text}");
    // The fake credentials never contain the real key.
    let models = std::fs::read_to_string(session_dir.join("agent/models.json").as_path()).unwrap();
    assert!(!models.contains("sk-test-key"));

    let sandbox = prepared.sandbox.unwrap();
    assert!(sandbox.hook.is_none());
    assert_eq!(sandbox.built.api_key_providers, ["mock"]);
}

#[test]
fn webhook_port_enables_approvals() {
    let f = fixture("");
    let prepared = prepare(&f.host, &launch(&f, None, Some(47100))).unwrap();
    let hook = prepared.sandbox.unwrap().hook.unwrap();
    assert_eq!(hook.port, 47100);
    assert_eq!(hook.session.as_str(), SESSION);
    let profile = std::fs::read_to_string(
        f.host
            .dirs
            .state()
            .join(&format!("sessions/{SESSION}/profile.json"))
            .as_path(),
    )
    .unwrap();
    assert!(profile.contains(&hook.url()));
    assert!(profile.contains("\"capability_elevation\": true"));
}

#[test]
fn level_off_runs_the_login_shell_directly() {
    let mut f = fixture("");
    f.host.nono = None;
    let prepared = prepare(&f.host, &launch(&f, Some(SandboxLevel::Off), None)).unwrap();
    assert_eq!(
        argv(&prepared),
        ["/bin/sh", "-lc", "exec pi --mode rpc --session-id 'a b'"]
    );
    assert!(prepared.request.spec.env_file.is_none());
    assert!(prepared.sandbox.is_none());
    assert_eq!(prepared.request.spec.labels["sandbox"], "off");
}

#[test]
fn sandboxed_levels_need_nono() {
    let mut f = fixture("");
    f.host.nono = None;
    for level in [None, Some(SandboxLevel::Relaxed)] {
        assert!(matches!(
            prepare(&f.host, &launch(&f, level, None)),
            Err(LaunchError::NoNono)
        ));
    }
}

#[test]
fn rejected_policies_write_nothing_runnable() {
    let f = fixture("");
    let mut l = launch(&f, None, None);
    // A grant covering the daemon's state dir is refused by the profile checks.
    l.policy.paths.push(controlroom_sandbox::policy::PathGrant {
        path: f.host.dirs.home().clone(),
        kind: controlroom_sandbox::policy::PathKind::Dir,
        access: controlroom_sandbox::policy::Access::ReadWrite,
    });
    l.policy.commands.push(CommandRule {
        command: Name::new("kubectl").unwrap(),
        args: vec!["apply".into()],
        decision: CommandDecision::Approve,
    });
    assert!(matches!(prepare(&f.host, &l), Err(LaunchError::Profile(_))));
    assert!(
        !f.host
            .dirs
            .state()
            .join(&format!("sessions/{SESSION}/profile.json"))
            .as_path()
            .exists()
    );
}
