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
        allowed_providers: None,
        model_defaults: ModelDefaults::default(),
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
    assert_eq!(prepared.built.api_key_providers, ["mock"]);
}

/// The server sandbox is always on: at level off pi runs under nono on its session agent dir,
/// with the same profile (routes included) as at strict, never on the daemon's own agent dir.
#[test]
fn level_off_runs_pi_under_the_same_server_sandbox() {
    let f = fixture(LOOPBACK_ROUTE);
    prepare(&f.host, &launch(SandboxLevel::Strict)).unwrap();
    let strict = profile(&f);
    let token = load_agent_token(&f.host.dirs, SESSION).unwrap();

    let prepared = prepare(&f.host, &launch(SandboxLevel::Off)).unwrap();
    let argv = argv(&prepared);
    assert_eq!(argv[0], "/nix/store/aaaa-nono-0.80.0/bin/nono");
    assert_eq!(argv[argv.len() - 3..], ["/bin/sh", "-lc", COMMAND]);
    assert_eq!(prepared.request.spec.labels["sandbox"], "off");
    assert_eq!(profile(&f), strict);
    let session_agent = f
        .host
        .dirs
        .state()
        .join(&format!("sessions/{SESSION}/agent"));
    let env = &strict["environment"].to_string();
    assert!(env.contains(session_agent.as_str()), "{env}");
    assert_eq!(prepared.built.api_key_providers, ["mock"]);
    let env = std::fs::read_to_string(prepared.request.spec.env_file.as_ref().unwrap()).unwrap();
    let env = controlroom_supervisor::parse_env(&env).unwrap();
    assert!(
        env.iter().all(|(k, _)| k != "PI_CODING_AGENT_DIR"),
        "{env:?}"
    );
    // Every launch gets a fresh agent token.
    assert!(
        !load_agent_token(&f.host.dirs, SESSION)
            .unwrap()
            .matches(&token)
    );
}

#[test]
fn every_level_needs_nono() {
    let mut f = fixture("");
    f.host.nono = None;
    for level in [
        SandboxLevel::Off,
        SandboxLevel::Strict,
        SandboxLevel::Relaxed,
    ] {
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

#[test]
fn a_model_allow_list_keeps_only_its_providers_routes_and_credentials() {
    use controlroom_sandbox::agent_dir::ManagedProviders;
    // Two providers with credentials: `mock` (the server's models.json key) and `unused` (a
    // stored key), each with a route. The session's profile allows `mock` only.
    let mut f = fixture(LOOPBACK_ROUTE);
    f.host.sandbox.agent_dir.managed = ManagedProviders {
        key_providers: vec!["unused".into()],
        key_store: Some(f.host.dirs.models().join("keys.json")),
        ..ManagedProviders::default()
    };
    let session_dir = f.host.dirs.state().join("sessions").join(SESSION);
    let layout = SessionLayout::new(f.host.dirs.state(), &SessionId::new(SESSION).unwrap());
    for level in [SandboxLevel::Strict, SandboxLevel::Off] {
        let restricted = AgentLaunch {
            allowed_providers: Some(ProviderAllowList::new(["mock"])),
            ..launch(level)
        };
        let prepared = prepare(&f.host, &restricted).unwrap();
        assert_eq!(prepared.built.api_key_providers, ["mock"]);
        assert!(prepared.built.stored_key_providers.is_empty());
        let profile = profile(&f);
        let routes = profile["network"]["custom_credentials"]
            .as_object()
            .unwrap();
        assert_eq!(routes.keys().collect::<Vec<_>>(), ["cr_mock"], "{level:?}");
        let auth = std::fs::read_to_string(session_dir.join("agent/auth.json").as_path()).unwrap();
        assert!(!auth.contains("unused"), "{auth}");
        let manifest = controlroom_sandbox::credential::load_manifest(&layout).unwrap();
        assert!(manifest.stored_keys.unwrap().providers.is_empty());
    }

    // A profile that admits no account (unreported): no provider, no route.
    let none = AgentLaunch {
        allowed_providers: Some(ProviderAllowList::default()),
        ..launch(SandboxLevel::Strict)
    };
    let prepared = prepare(&f.host, &none).unwrap();
    assert!(prepared.built.api_key_providers.is_empty());
    assert!(profile(&f)["network"]["custom_credentials"].is_null());
}

#[test]
fn the_profiles_model_defaults_go_on_top_unless_refused() {
    let f = fixture("");
    std::fs::write(
        f.host.dirs.pi_agent_dir().join("settings.json").as_path(),
        r#"{"defaultProvider": "mock", "defaultModel": "m", "defaultThinkingLevel": "low"}"#,
    )
    .unwrap();
    let settings = || -> Value {
        let path = f
            .host
            .dirs
            .state()
            .join(&format!("sessions/{SESSION}/agent/settings.json"));
        serde_json::from_str(&std::fs::read_to_string(path.as_path()).unwrap()).unwrap()
    };
    let defaults = ModelDefaults {
        provider: Some("mock".into()),
        model: Some("m2".into()),
        thinking: None,
    };
    let with_defaults = AgentLaunch {
        model_defaults: defaults.clone(),
        ..launch(SandboxLevel::Strict)
    };
    prepare(&f.host, &with_defaults).unwrap();
    let s = settings();
    assert_eq!(
        (
            &s["defaultProvider"],
            &s["defaultModel"],
            &s["defaultThinkingLevel"]
        ),
        (&"mock".into(), &"m2".into(), &"low".into())
    );

    // A default of a provider the profile refuses is dropped: pi starts on an allowed one.
    let refused = AgentLaunch {
        allowed_providers: Some(ProviderAllowList::new(["other"])),
        ..with_defaults
    };
    prepare(&f.host, &refused).unwrap();
    let s = settings();
    assert!(
        s.get("defaultProvider").is_none() && s.get("defaultModel").is_none(),
        "{s}"
    );
}
