//! `controlroom-agentd credential` end to end: a built session dir, the real binary.

use std::process::Command;

use controlroom_sandbox::agent_dir::{AgentDirSpec, build_session_dirs};
use controlroom_sandbox::{AbsPath, SessionId, SessionLayout};

const SESSION: &str = "01HZY3K4M5N6P7Q8R9S0T1V2W3";

fn controlroomd(state: &AbsPath, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_controlroom-agentd"))
        .args(["credential", "--state-dir", state.as_str()])
        .args(args)
        .env_remove("XDG_DATA_HOME")
        .output()
        .expect("run controlroom-agentd")
}

/// A user agent dir with an API key and an OAuth credential, and a session built from it.
/// `helper` is the OAuth helper's script body, if there is one.
fn session(root: &AbsPath, helper: Option<&str>) -> AbsPath {
    let user = root.join("user-agent");
    std::fs::create_dir_all(user.as_path()).expect("mkdir");
    std::fs::write(
        user.join("auth.json").as_path(),
        serde_json::json!({
            "gateway": {"type": "api_key", "key": "!printf sk-test-key"},
            "sub": {"type": "oauth", "access": "x", "refresh": "y", "expires": 1}
        })
        .to_string(),
    )
    .expect("write auth");
    let oauth_helper = helper.map(|body| {
        let path = root.join("controlroom-pi-oauth");
        // Written from a child shell: writing it here would race with the other tests' forks
        // (a child inheriting the write fd makes executing the script fail with ETXTBSY).
        let content = format!("#!/bin/sh\necho \"$@\" > {root}/helper-args\n{body}\n");
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"printf '%s' "$1" > "$2" && chmod 755 "$2""#)
            .arg("sh")
            .arg(&content)
            .arg(path.as_path())
            .status()
            .expect("write helper");
        assert!(status.success(), "write helper");
        path
    });
    let state = root.join("state");
    let layout = SessionLayout::new(&state, &SessionId::new(SESSION).expect("id"));
    build_session_dirs(
        &layout,
        &AgentDirSpec {
            user_agent_dir: Some(user),
            oauth_helper,
            ..AgentDirSpec::default()
        },
    )
    .expect("build session dirs");
    state
}

fn assert_fails_closed(state: &AbsPath, args: &[&str]) {
    let out = controlroomd(state, args);
    assert!(!out.status.success(), "{args:?}");
    assert!(out.stdout.is_empty(), "{args:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("sk-test") && !stderr.contains("access-token"),
        "{stderr}"
    );
}

#[test]
fn prints_the_key_without_newline_and_fails_closed() {
    let t = tempfile::tempdir().expect("tempdir");
    let root = AbsPath::from_path(t.path()).expect("utf-8 tempdir");
    let state = session(&root, None);

    let ok = controlroomd(&state, &[SESSION, "gateway"]);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert_eq!(ok.stdout, b"sk-test-key");

    for args in [
        [SESSION, "sub"],                          // OAuth without the helper
        [SESSION, "missing"],                      // unknown provider
        ["01HZY3K4M5N6P7Q8R9S0T1V2W4", "gateway"], // unknown session
        ["../../etc", "gateway"],                  // not a session id
        [SESSION, "../x"],                         // not a provider name
    ] {
        assert_fails_closed(&state, &args);
    }
    let out = controlroomd(&state, &[SESSION, "sub"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("controlroom-pi-oauth"));
}

#[test]
fn oauth_tokens_come_from_the_helper() {
    let t = tempfile::tempdir().expect("tempdir");
    let root = AbsPath::from_path(t.path()).expect("utf-8 tempdir");
    let state = session(&root, Some("printf 'access-token\\n'"));

    let ok = controlroomd(&state, &["--valid-for-secs", "900", SESSION, "sub"]);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert_eq!(ok.stdout, b"access-token");
    let args = std::fs::read_to_string(root.join("helper-args").as_path()).expect("args");
    assert_eq!(
        args.trim(),
        format!(
            "--agent-dir {} --provider sub --valid-for-secs 900",
            root.join("user-agent")
        )
    );
    // API keys don't go through the helper.
    assert_eq!(
        controlroomd(&state, &[SESSION, "gateway"]).stdout,
        b"sk-test-key"
    );
}

#[test]
fn a_failing_oauth_helper_fails_closed_with_its_message() {
    let t = tempfile::tempdir().expect("tempdir");
    let root = AbsPath::from_path(t.path()).expect("utf-8 tempdir");
    let state = session(
        &root,
        Some("echo 'controlroom-pi-oauth: refreshing sub failed: invalid_grant' >&2; exit 1"),
    );
    assert_fails_closed(&state, &[SESSION, "sub"]);
    let out = controlroomd(&state, &[SESSION, "sub"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid_grant"));
}

/// Keys added in Settings › Models: the launch merge gives each preset its route (origin and
/// header as pi sends the key) and the session's manifest points the helper at the key store.
#[test]
fn stored_keys_resolve_per_preset_route() {
    use controlroom_agentd::config::sandbox as sandbox_config;
    use controlroom_agentd::dirs::Dirs;
    use controlroom_agentd::models::merge;
    use controlroom_sandbox::credential::KeyStore;

    let t = tempfile::tempdir().expect("tempdir");
    let root = AbsPath::from_path(t.path()).expect("utf-8 tempdir");
    let dirs = Dirs::under(root.join("state"));
    std::fs::create_dir_all(dirs.models().as_path()).expect("mkdir");
    let mut keys = KeyStore::default();
    for (provider, key) in [
        ("anthropic", "sk-ant-stored"),
        ("openai", "sk-openai-stored"),
        ("google", "google-stored"),
        ("openrouter", "sk-or-stored"),
    ] {
        keys.insert(provider, key);
    }
    keys.save(&dirs.models().join("keys.json"))
        .expect("save keys");

    let mut sandbox = sandbox_config::SandboxConfig::default()
        .resolve(None, &dirs.pi_agent_dir())
        .expect("sandbox");
    merge::apply(&dirs, &mut sandbox);
    let routes: Vec<_> = sandbox
        .routes
        .iter()
        .map(|r| {
            (
                r.provider.as_str().to_owned(),
                r.upstream.as_str().to_owned(),
                r.inject_header.clone().unwrap_or_default(),
                r.credential_format.clone().unwrap_or_default(),
            )
        })
        .collect();
    let route = |p: &str, u: &str, h: &str, f: &str| {
        (p.to_owned(), u.to_owned(), h.to_owned(), f.to_owned())
    };
    assert_eq!(
        routes,
        [
            route("anthropic", "https://api.anthropic.com", "x-api-key", "{}"),
            route(
                "openai",
                "https://api.openai.com",
                "Authorization",
                "Bearer {}"
            ),
            route(
                "google",
                "https://generativelanguage.googleapis.com",
                "x-goog-api-key",
                "{}"
            ),
            route(
                "openrouter",
                "https://openrouter.ai",
                "Authorization",
                "Bearer {}"
            ),
        ]
    );

    let layout = SessionLayout::new(dirs.state(), &SessionId::new(SESSION).expect("id"));
    build_session_dirs(&layout, &sandbox.agent_dir).expect("build session dirs");
    for (provider, key) in [("anthropic", "sk-ant-stored"), ("google", "google-stored")] {
        let out = controlroomd(dirs.state(), &[SESSION, provider]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout, key.as_bytes());
    }
    // The session's agent dir holds placeholders only.
    let auth = std::fs::read_to_string(layout.agent().join("auth.json").as_path()).expect("auth");
    assert!(!auth.contains("stored"), "{auth}");
    assert_fails_closed(dirs.state(), &[SESSION, "openai-codex"]);
}

/// A subscription token pasted for Anthropic in Settings › Models: the session gets the sign-in's
/// Bearer route and a stand-in with the token's type prefix, and the helper prints the stored
/// token as is (it is never refreshed).
#[test]
fn a_pasted_subscription_token_is_printed_as_is() {
    use controlroom_agentd::config::sandbox as sandbox_config;
    use controlroom_agentd::dirs::Dirs;
    use controlroom_agentd::models::merge;
    use controlroom_sandbox::agent_dir::PLACEHOLDER;
    use controlroom_sandbox::credential::KeyStore;

    let t = tempfile::tempdir().expect("tempdir");
    let root = AbsPath::from_path(t.path()).expect("utf-8 tempdir");
    let dirs = Dirs::under(root.join("state"));
    std::fs::create_dir_all(dirs.models().as_path()).expect("mkdir");
    let token = "sk-ant-oat01-test-secret-abcd";
    let mut keys = KeyStore::default();
    keys.insert("anthropic", token);
    keys.save(&dirs.models().join("keys.json"))
        .expect("save keys");

    let mut sandbox = sandbox_config::SandboxConfig::default()
        .resolve(None, &dirs.pi_agent_dir())
        .expect("sandbox");
    merge::apply(&dirs, &mut sandbox);
    let route = &sandbox.routes[0];
    assert_eq!(
        (
            route.provider.as_str(),
            route.upstream.as_str(),
            route.inject_header.as_deref(),
            route.credential_format.as_deref(),
        ),
        (
            "anthropic",
            "https://api.anthropic.com",
            Some("Authorization"),
            Some("Bearer {}")
        )
    );

    let layout = SessionLayout::new(dirs.state(), &SessionId::new(SESSION).expect("id"));
    build_session_dirs(&layout, &sandbox.agent_dir).expect("build session dirs");
    let auth = std::fs::read_to_string(layout.agent().join("auth.json").as_path()).expect("auth");
    let auth: serde_json::Value = serde_json::from_str(&auth).expect("auth.json");
    assert_eq!(
        auth["anthropic"],
        serde_json::json!({"type": "api_key", "key": format!("sk-ant-oat01-{PLACEHOLDER}")})
    );
    let out = controlroomd(dirs.state(), &[SESSION, "anthropic"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, token.as_bytes());
}
