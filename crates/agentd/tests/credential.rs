//! `controlroom-agentd credential` end to end: a built session dir, the real binary.

use std::os::unix::fs::PermissionsExt;
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
        std::fs::write(
            path.as_path(),
            format!("#!/bin/sh\necho \"$@\" > {root}/helper-args\n{body}\n"),
        )
        .expect("write helper");
        std::fs::set_permissions(path.as_path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
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
