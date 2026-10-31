//! `controlroomd credential` end to end: a built session dir, the real binary.

use std::process::Command;

use controlroom_sandbox::agent_dir::{AgentDirSpec, build_session_dirs};
use controlroom_sandbox::{AbsPath, SessionId, SessionLayout};

const SESSION: &str = "01HZY3K4M5N6P7Q8R9S0T1V2W3";

fn controlroomd(state: &AbsPath, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_controlroomd"))
        .args(["credential", "--state-dir", state.as_str()])
        .args(args)
        .env_remove("XDG_DATA_HOME")
        .output()
        .expect("run controlroomd")
}

#[test]
fn prints_the_key_without_newline_and_fails_closed() {
    let t = tempfile::tempdir().expect("tempdir");
    let root = AbsPath::from_path(t.path()).expect("utf-8 tempdir");
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
    let state = root.join("state");
    let layout = SessionLayout::new(&state, &SessionId::new(SESSION).expect("id"));
    build_session_dirs(
        &layout,
        &AgentDirSpec {
            user_agent_dir: Some(user),
            ..AgentDirSpec::default()
        },
    )
    .expect("build session dirs");

    let ok = controlroomd(&state, &[SESSION, "gateway"]);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert_eq!(ok.stdout, b"sk-test-key");

    for args in [
        [SESSION, "sub"],                          // OAuth: not served here
        [SESSION, "missing"],                      // unknown provider
        ["01HZY3K4M5N6P7Q8R9S0T1V2W4", "gateway"], // unknown session
        ["../../etc", "gateway"],                  // not a session id
        [SESSION, "../x"],                         // not a provider name
    ] {
        let out = controlroomd(&state, &args);
        assert!(!out.status.success(), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(!String::from_utf8_lossy(&out.stderr).contains("sk-test"));
    }
}
