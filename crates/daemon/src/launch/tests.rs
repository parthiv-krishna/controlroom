use std::os::unix::fs::PermissionsExt;

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
        oauth_helper: None,
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
    // @controlroom/pi finds its own socket and the launch's token.
    let agent_socket = state.join(&format!("run/{SESSION}.agent.sock"));
    assert_eq!(get("CONTROLROOM_SESSION_ID"), Some(SESSION));
    assert_eq!(get("CONTROLROOM_AGENT_SOCKET"), Some(agent_socket.as_str()));
    let token = load_agent_token(&f.host.dirs, SESSION).unwrap();
    assert_eq!(get("CONTROLROOM_AGENT_TOKEN"), Some(token.as_str()));

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
    // The sandbox may reach its own agent socket, and no other.
    assert_eq!(
        profile["filesystem"]["unix_socket"],
        serde_json::json!([agent_socket.as_str()])
    );
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

    // The listener finds this launch's secret, private to the user; each launch gets a new
    // one, and a launch without a webhook leaves none behind.
    let stored = load_hook_secret(&f.host.dirs, SESSION).unwrap();
    assert!(stored.matches(&hook.secret));
    let path = f.host.dirs.hook_secret(SESSION);
    let mode = std::fs::metadata(path.as_path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let again = prepare(&f.host, &launch(&f, None, Some(47100))).unwrap();
    let next = again.sandbox.unwrap().hook.unwrap();
    assert!(!next.secret.matches(&hook.secret));
    assert!(
        load_hook_secret(&f.host.dirs, SESSION)
            .unwrap()
            .matches(&next.secret)
    );
    prepare(&f.host, &launch(&f, Some(SandboxLevel::Off), None)).unwrap();
    assert!(load_hook_secret(&f.host.dirs, SESSION).is_none());
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
    // The env file only carries what @controlroom/pi needs; a new launch has a new token.
    let env = std::fs::read_to_string(prepared.request.spec.env_file.as_ref().unwrap()).unwrap();
    let env = controlroom_supervisor::parse_env(&env).unwrap();
    let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "CONTROLROOM_SESSION_ID",
            "CONTROLROOM_AGENT_SOCKET",
            "CONTROLROOM_AGENT_TOKEN"
        ]
    );
    let token = load_agent_token(&f.host.dirs, SESSION).unwrap();
    prepare(&f.host, &launch(&f, Some(SandboxLevel::Off), None)).unwrap();
    assert!(
        !load_agent_token(&f.host.dirs, SESSION)
            .unwrap()
            .matches(&token)
    );
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

#[test]
fn existing_exec_dirs_reach_the_profile_with_mediation() {
    let mut f = fixture("");
    let tools = f.root.join("tools");
    std::fs::create_dir_all(tools.as_path()).unwrap();
    f.host.sandbox.exec_dirs = vec![tools.clone(), f.root.join("missing")];
    // A `git` on PATH turns mediation (and with it nono's exec gate) on.
    let bin = f.root.join("bin");
    std::fs::create_dir_all(bin.as_path()).unwrap();
    let git = bin.join("git");
    std::fs::write(git.as_path(), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(git.as_path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    f.host.path_var = bin.to_string();

    prepare(&f.host, &launch(&f, None, None)).unwrap();
    let path = f
        .host
        .dirs
        .state()
        .join(&format!("sessions/{SESSION}/profile.json"));
    let profile: Value =
        serde_json::from_str(&std::fs::read_to_string(path.as_path()).unwrap()).unwrap();
    assert_eq!(
        profile["command_policies"]["executable_dirs"],
        serde_json::json!([tools.as_str()])
    );
}

#[test]
fn oauth_providers_get_routes_with_the_helper_and_npm_is_readable() {
    let toml = r#"
npm = "~/.pi/agent/npm"
[[routes]]
provider = "sub"
upstream = "https://api.example.com"
allow = ["POST /v1/responses"]
"#;
    let mut f = fixture(toml);
    let user_agent = f.host.dirs.home().join(".pi/agent");
    std::fs::write(
        user_agent.join("auth.json").as_path(),
        r#"{"sub": {"type": "oauth", "access": "a", "refresh": "r", "expires": 1}}"#,
    )
    .unwrap();
    let profile = |f: &Fixture| -> Value {
        prepare(&f.host, &launch(f, None, None)).unwrap();
        let path = f
            .host
            .dirs
            .state()
            .join(&format!("sessions/{SESSION}/profile.json"));
        serde_json::from_str(&std::fs::read_to_string(path.as_path()).unwrap()).unwrap()
    };

    // Without the helper the token can't be served, so there is no route.
    let without = profile(&f);
    assert!(
        without["network"]["custom_credentials"]
            .get("cr_sub")
            .is_none()
    );

    f.host.oauth_helper =
        Some(AbsPath::new("/nix/store/aaaa-controlroom-pi/bin/controlroom-pi-oauth").unwrap());
    let with = profile(&f);
    let capture = &with["credential_capture"]["cr_sub"];
    let command: Vec<&str> = capture["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(command[4..7], ["--valid-for-secs", "600", SESSION]);
    assert_eq!(capture["timeout_secs"], 60);
    // The manifest tells `controlroomd credential` where the helper is.
    let manifest = std::fs::read_to_string(
        f.host
            .dirs
            .state()
            .join(&format!("sessions/{SESSION}/credentials.json"))
            .as_path(),
    )
    .unwrap();
    assert!(manifest.contains("controlroom-pi-oauth"), "{manifest}");

    // The user's installed pi packages behind agent/npm are readable, not writable.
    let read = with["filesystem"]["read"].as_array().unwrap();
    assert!(read.contains(&Value::String(user_agent.join("npm").to_string())));
}
