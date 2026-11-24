//! A profile's model allow-list against real pi (design `docs/design/profiles-tenants.md` §2,
//! A1 and A4): the agent daemon's pi config has two providers, `local` and `paid`, each pointing
//! at its own mock model (the fixture tool's). A launch whose session allows only `local` builds
//! a session agent dir in which pi lists no `paid` model, refuses `--provider paid`, and never
//! calls the `paid` mock; the same launch without the list reaches both.
//!
//! The launches are prepared at level off, which builds the session agent dir as every level
//! does (pi always runs under nono on the server). pi then runs directly on that agent dir, not
//! under nono (the credential routes, which the list also narrows, are checked by `launch`'s
//! unit tests); the mocks don't check keys.
//!
//! Needs the bundle, so it is `#[ignore]`d; the flake check `agentd-pi-model-policy` runs it
//! with `CR_TEST_PI_BUNDLE` set (or: `nix build -o /tmp/pi .#controlroom-pi` and
//! `CR_TEST_PI_BUNDLE=/tmp/pi cargo test -p controlroom-agentd --test pi_model_policy --
//! --ignored`). Every pi is waited for or killed; the mocks stop with the test.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use controlroom_agentd::config::agents::AgentEntry;
use controlroom_agentd::config::sandbox::SandboxConfig;
use controlroom_agentd::dirs::Dirs;
use controlroom_agentd::launch::{AgentLaunch, Host, prepare};
use controlroom_fixture_tool::mock_model::MockModel;
use controlroom_protocol::agentd::ModelDefaults;
use controlroom_protocol::session::DriverKind;
use controlroom_sandbox::agent_dir::ProviderAllowList;
use controlroom_sandbox::policy::SandboxLevel;
use controlroom_sandbox::{AbsPath, SessionId, SessionLayout};
use serde_json::json;

const SESSION: &str = "01HZY3K4M5N6P7Q8R9S0T1V2W3";

/// How long pi may take for one command.
const PI_TIMEOUT: Duration = Duration::from_secs(120);

/// Kills the child when dropped.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The agent daemon's pi config: `local` and `paid`, one model each, on their mocks.
fn server_config(dirs: &Dirs, local: &MockModel, paid: &MockModel) {
    let agent = dirs.pi_agent_dir();
    std::fs::create_dir_all(agent.as_path()).expect("mkdir agent dir");
    let provider = |mock: &MockModel, key: &str| {
        json!({"baseUrl": mock.base_url(), "api": "openai-completions", "apiKey": key,
               "models": [{"id": "m", "name": "Mock", "input": ["text"], "contextWindow": 100000,
                           "maxTokens": 1000,
                           "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}}]})
    };
    let models = json!({"providers": {
        "local": provider(local, "sk-local-test"),
        "paid": provider(paid, "sk-paid-test"),
    }});
    std::fs::write(agent.join("models.json").as_path(), models.to_string())
        .expect("write models.json");
    std::fs::write(
        agent.join("settings.json").as_path(),
        json!({"defaultProvider": "paid", "defaultModel": "m"}).to_string(),
    )
    .expect("write settings.json");
}

/// Prepares the session's launch (level off: the agent dir is built all the same; nono itself
/// isn't run) and returns the session's agent dir.
fn launch(dirs: &Dirs, allowed: Option<ProviderAllowList>) -> AbsPath {
    let config: SandboxConfig = toml::from_str("").expect("sandbox config");
    let host = Host {
        sandbox: config
            .resolve(None, &dirs.pi_agent_dir())
            .expect("resolve sandbox config"),
        dirs: dirs.clone(),
        agentd: AbsPath::new("/nix/store/aaaa-controlroom/bin/controlroom-agentd").expect("path"),
        shell: AbsPath::new("/bin/sh").expect("path"),
        nono: Some(AbsPath::new("/nix/store/aaaa-nono-0.80.0/bin/nono").expect("path")),
        oauth_helper: None,
    };
    let session = SessionId::new(SESSION).expect("id");
    let launch = AgentLaunch {
        session: session.clone(),
        agent_name: "pi".into(),
        agent: AgentEntry {
            command: "controlroom-pi".into(),
            driver: DriverKind::Pi,
        },
        args: Vec::new(),
        level: SandboxLevel::Off,
        allowed_providers: allowed,
        model_defaults: ModelDefaults::default(),
    };
    prepare(&host, &launch).expect("prepare the launch");
    SessionLayout::new(dirs.state(), &session).agent()
}

/// Runs the bundle's pi with `args` on `agent`, in an isolated home; (success, stdout, stderr).
fn pi(bundle: &Path, agent: &AbsPath, home: &Path, args: &[&str]) -> (bool, String, String) {
    std::fs::create_dir_all(home).expect("mkdir home");
    let mut command = Command::new(bundle.join("bin/controlroom-pi"));
    command
        .arg("--")
        .args(args)
        .arg("--no-session")
        .current_dir(home)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
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
    let mut stdout = child.0.stdout.take().expect("stdout");
    let mut stderr = child.0.stderr.take().expect("stderr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut out, mut err) = (String::new(), String::new());
        let _ = stdout.read_to_string(&mut out);
        let _ = stderr.read_to_string(&mut err);
        let _ = tx.send((out, err));
    });
    let (out, err) = rx.recv_timeout(PI_TIMEOUT).expect("pi finishes");
    let status = child.0.wait().expect("wait for pi");
    (status.success(), out, err)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the controlroom-pi bundle (CR_TEST_PI_BUNDLE); run by the flake check agentd-pi-model-policy"]
async fn a_session_never_reaches_a_provider_outside_its_allow_list() {
    let bundle = std::env::var_os("CR_TEST_PI_BUNDLE")
        .expect("CR_TEST_PI_BUNDLE is the controlroom-pi bundle");
    let bundle = Path::new(&bundle).to_owned();
    let local = MockModel::start(Vec::new()).await.expect("local mock");
    let paid = MockModel::start(Vec::new()).await.expect("paid mock");
    let t = tempfile::tempdir().expect("tempdir");
    let root = AbsPath::from_path(t.path()).expect("utf-8 tempdir");
    let dirs = Dirs::under(root.join("state"));
    server_config(&dirs, &local, &paid);
    let home = t.path().join("home");
    let run = |agent: AbsPath, args: &'static [&'static str]| {
        let (bundle, home) = (bundle.clone(), home.clone());
        tokio::task::spawn_blocking(move || pi(&bundle, &agent, &home, args))
    };
    let prompt_paid: &[&str] = &["-p", "hi", "--provider", "paid", "--model", "m"];
    let prompt_local: &[&str] = &["-p", "hi", "--provider", "local", "--model", "m"];

    // Without a list, both providers are there (the control case).
    let open = launch(&dirs, None);
    let (ok, listed, err) = run(open.clone(), &["--list-models"]).await.expect("join");
    assert!(
        ok && listed.contains("local") && listed.contains("paid"),
        "{listed}{err}"
    );
    let (ok, out, err) = run(open, prompt_paid).await.expect("join");
    assert!(ok && out.trim() == "ok", "{out}{err}");
    let paid_calls = paid.requests().len();
    assert_eq!(paid_calls, 1);

    // The profile allows `local` only.
    let restricted = launch(&dirs, Some(ProviderAllowList::new(["local"])));
    let (ok, listed, err) = run(restricted.clone(), &["--list-models"])
        .await
        .expect("join");
    assert!(ok && listed.contains("local"), "{listed}{err}");
    assert!(!listed.contains("paid"), "{listed}");
    let (ok, out, err) = run(restricted.clone(), prompt_paid).await.expect("join");
    assert!(!ok, "pi used a refused provider: {out}{err}");
    let (ok, out, err) = run(restricted.clone(), prompt_local).await.expect("join");
    assert!(ok && out.trim() == "ok", "{out}{err}");
    // The default provider was `paid`: pi starts on an allowed one instead.
    let (ok, out, err) = run(restricted, &["-p", "hi"]).await.expect("join");
    assert!(ok && out.trim() == "ok", "{out}{err}");
    assert_eq!(
        paid.requests().len(),
        paid_calls,
        "the paid mock was called"
    );
    assert_eq!(local.requests().len(), 2);
}
