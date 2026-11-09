use controlroom_protocol::session::DriverKind;
use serde_json::Value;

use super::*;
use crate::config::sandbox::SandboxConfig;

const SESSION: &str = "01HZY3K4M5N6P7Q8R9S0T1V2W3";

struct Fixture {
    _tmp: tempfile::TempDir,
    host: Host,
}

fn fixture(sandbox_toml: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let state = AbsPath::from_path(tmp.path()).unwrap().join("state");
    let dirs = Dirs::under(state);
    let agent_dir = dirs.pi_agent_dir();
    std::fs::create_dir_all(agent_dir.as_path()).unwrap();
    std::fs::write(
        agent_dir.join("models.json").as_path(),
        serde_json::json!({"providers": {"mock": {
            "baseUrl": "http://127.0.0.1:4000/v1", "api": "openai-completions",
            "apiKey": "sk-test-key", "models": [{"id": "m"}]
        }}})
        .to_string(),
    )
    .unwrap();
    let config: SandboxConfig = toml::from_str(sandbox_toml).unwrap();
    let host = Host {
        sandbox: config.resolve(None, &agent_dir).unwrap(),
        dirs,
        agentd: AbsPath::new("/nix/store/aaaa-controlroom/bin/controlroom-agentd").unwrap(),
        shell: AbsPath::new("/bin/sh").unwrap(),
        nono: Some(AbsPath::new("/nix/store/aaaa-nono-0.80.0/bin/nono").unwrap()),
        oauth_helper: None,
    };
    Fixture { _tmp: tmp, host }
}

fn launch(level: SandboxLevel) -> AgentLaunch {
    AgentLaunch {
        session: SessionId::new(SESSION).unwrap(),
        agent_name: "pi".into(),
        agent: AgentEntry {
            command: "controlroom-pi".into(),
            driver: DriverKind::Pi,
        },
        args: vec![
            "--mode".into(),
            "rpc".into(),
            "--session-id".into(),
            "a b".into(),
        ],
        level,
    }
}

fn argv(prepared: &PreparedLaunch) -> &[String] {
    match &prepared.request.spec.exec {
        Exec::Argv(argv) => argv,
        Exec::Shell(_) => panic!("expected an argv"),
    }
}

fn profile(f: &Fixture) -> Value {
    let path = f
        .host
        .dirs
        .state()
        .join(&format!("sessions/{SESSION}/profile.json"));
    serde_json::from_str(&std::fs::read_to_string(path.as_path()).unwrap()).unwrap()
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

const COMMAND: &str = "exec controlroom-pi --no-builtin-tools --no-context-files --no-skills \
                       --mode rpc --session-id 'a b'";

#[test]
fn strict_launch_runs_pi_in_an_empty_workdir_with_remote_tools_only() {
    let f = fixture(LOOPBACK_ROUTE);
    let prepared = prepare(&f.host, &launch(SandboxLevel::Strict)).unwrap();
    let state = f.host.dirs.state();
    let session_dir = state.join("sessions").join(SESSION);

    let argv = argv(&prepared);
    assert_eq!(argv[0], "/nix/store/aaaa-nono-0.80.0/bin/nono");
    let tail = &argv[argv.len() - 3..];
    assert_eq!(tail, ["/bin/sh", "-lc", COMMAND]);

    let request = &prepared.request;
    assert_eq!(request.id, SESSION);
    assert_eq!(request.paths.dir, session_dir.as_path());
    request.paths.validate().unwrap();
    request.spec.validate().unwrap();
    let workdir = session_dir.join("work");
    assert_eq!(request.spec.cwd, workdir.as_path());
    assert_eq!(std::fs::read_dir(workdir.as_path()).unwrap().count(), 0);
    assert_eq!(request.spec.labels["sandbox"], "strict");

    let env = std::fs::read_to_string(request.spec.env_file.as_ref().unwrap()).unwrap();
    let env = controlroom_supervisor::parse_env(&env).unwrap();
    let get = |k: &str| {
        env.iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(get("TMPDIR"), Some("/tmp"));
    let agent_socket = state.join(&format!("run/{SESSION}.agent.sock"));
    assert_eq!(get("CONTROLROOM_SESSION_ID"), Some(SESSION));
    assert_eq!(get("CONTROLROOM_AGENT_SOCKET"), Some(agent_socket.as_str()));
    let token = load_agent_token(&f.host.dirs, SESSION).unwrap();
    assert_eq!(get("CONTROLROOM_AGENT_TOKEN"), Some(token.as_str()));

    let profile = profile(&f);
    // Only providers with a key get a route; the helper is this agent daemon.
    let routes = profile["network"]["custom_credentials"]
        .as_object()
        .unwrap();
    assert_eq!(routes.keys().collect::<Vec<_>>(), ["cr_mock"]);
    let helper = &profile["credential_capture"]["cr_mock"]["command"];
    assert_eq!(
        helper[0],
        "/nix/store/aaaa-controlroom/bin/controlroom-agentd"
    );
    assert_eq!(helper[1], "credential");
    assert_eq!(helper[3], state.as_str());
    // pi runs no commands here: nothing mediated, nothing to approve.
    let text = profile.to_string();
    assert!(
        !text.contains("approve") && !text.contains("command_policies"),
        "{text}"
    );
    // The sandbox may reach its own agent socket, and no other.
    assert_eq!(
        profile["filesystem"]["unix_socket"],
        serde_json::json!([agent_socket.as_str()])
    );
    let models = std::fs::read_to_string(session_dir.join("agent/models.json").as_path()).unwrap();
    assert!(!models.contains("sk-test-key"));
    assert_eq!(prepared.built.unwrap().api_key_providers, ["mock"]);
}

#[test]
fn level_off_runs_the_login_shell_directly() {
    let mut f = fixture("");
    f.host.nono = None;
    let prepared = prepare(&f.host, &launch(SandboxLevel::Off)).unwrap();
    assert_eq!(argv(&prepared), ["/bin/sh", "-lc", COMMAND]);
    let env = std::fs::read_to_string(prepared.request.spec.env_file.as_ref().unwrap()).unwrap();
    let env = controlroom_supervisor::parse_env(&env).unwrap();
    let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "CONTROLROOM_SESSION_ID",
            "CONTROLROOM_AGENT_SOCKET",
            "CONTROLROOM_AGENT_TOKEN",
            "PI_CODING_AGENT_DIR"
        ]
    );
    let token = load_agent_token(&f.host.dirs, SESSION).unwrap();
    prepare(&f.host, &launch(SandboxLevel::Off)).unwrap();
    assert!(
        !load_agent_token(&f.host.dirs, SESSION)
            .unwrap()
            .matches(&token)
    );
    assert!(prepared.built.is_none());
}

#[test]
fn sandboxed_levels_need_nono() {
    let mut f = fixture("");
    f.host.nono = None;
    for level in [SandboxLevel::Strict, SandboxLevel::Relaxed] {
        assert!(matches!(
            prepare(&f.host, &launch(level)),
            Err(LaunchError::NoNono)
        ));
    }
}

#[test]
fn oauth_providers_get_routes_with_the_helper_and_npm_is_readable() {
    let toml = r#"
npm = "/srv/pi-packages"
[[routes]]
provider = "sub"
upstream = "https://api.example.com"
allow = ["POST /v1/responses"]
"#;
    let mut f = fixture(toml);
    std::fs::write(
        f.host.dirs.pi_agent_dir().join("auth.json").as_path(),
        r#"{"sub": {"type": "oauth", "access": "a", "refresh": "r", "expires": 1}}"#,
    )
    .unwrap();
    prepare(&f.host, &launch(SandboxLevel::Strict)).unwrap();
    assert!(
        profile(&f)["network"]["custom_credentials"]
            .get("cr_sub")
            .is_none(),
        "no helper, no route"
    );
    f.host.oauth_helper =
        Some(AbsPath::new("/nix/store/aaaa-controlroom-pi/bin/controlroom-pi-oauth").unwrap());
    prepare(&f.host, &launch(SandboxLevel::Strict)).unwrap();
    let with = profile(&f);
    let capture = &with["credential_capture"]["cr_sub"];
    let command: Vec<&str> = capture["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(command[4..7], ["--valid-for-secs", "600", SESSION]);
    let read = with["filesystem"]["read"].as_array().unwrap();
    assert!(read.contains(&Value::String("/srv/pi-packages".into())));
}

#[test]
fn stored_keys_and_gateways_get_routes() {
    use controlroom_sandbox::agent_dir::ManagedProviders;
    let mut f = fixture(LOOPBACK_ROUTE);
    let store = f.host.dirs.models().join("keys.json");
    f.host.sandbox.agent_dir.managed = ManagedProviders {
        key_providers: vec!["unused".into()],
        key_store: Some(store.clone()),
        ..ManagedProviders::default()
    };
    prepare(&f.host, &launch(SandboxLevel::Strict)).unwrap();
    let profile = profile(&f);
    let routes = profile["network"]["custom_credentials"]
        .as_object()
        .unwrap();
    assert_eq!(routes.keys().collect::<Vec<_>>(), ["cr_mock", "cr_unused"]);
    let layout = SessionLayout::new(f.host.dirs.state(), &SessionId::new(SESSION).unwrap());
    let manifest = controlroom_sandbox::credential::load_manifest(&layout).unwrap();
    assert_eq!(manifest.stored_keys.unwrap().path, store);
}
