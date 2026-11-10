//! The user's own terminals: opened in both modes, out of the agent's reach, never reopened,
//! ended with their workspace, and journaled across a daemon restart.

use controlroom_protocol::ErrorData;
use controlroom_protocol::host::{
    HostShell, ShellCloseParams, ShellListParams, ShellOpenParams, WorkspaceCloseParams, methods,
};
use controlroom_protocol::session::{SandboxLevel, ShellOwner, ShellSandbox, ShellState};

use crate::harness::{HostEnv, RpcClient, Session, bash_op, type_into, wait_until};

/// The shells the host lists, by name.
async fn listed(client: &RpcClient) -> Vec<HostShell> {
    client
        .call::<methods::ShellList>(ShellListParams { workspace_id: None })
        .await
        .unwrap()
        .shells
}

fn names(shells: &[HostShell]) -> Vec<&str> {
    shells.iter().map(|s| s.shell.name.as_str()).collect()
}

/// Waits for `shell.changed` saying `name` is gone.
async fn gone(client: &RpcClient, name: &str) {
    loop {
        let changed = client.note("shell.changed").await;
        if changed["shell"]["name"] == name && changed["shell"]["state"]["state"] == "gone" {
            return;
        }
    }
}

#[tokio::test]
async fn user_terminals_open_in_both_modes_and_the_agent_cant_reach_them() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    session.shell(&client, "devbox").await;

    let mine = session
        .user_shell(&client, "you@devbox", ShellSandbox::Workspace)
        .await
        .unwrap();
    assert_eq!(
        (mine.shell.owner, mine.shell.sandbox, &mine.shell.state),
        (ShellOwner::User, ShellSandbox::Workspace, &ShellState::Idle)
    );
    let raw = session
        .user_shell(&client, "you@devbox-2", ShellSandbox::Unsandboxed)
        .await
        .unwrap();
    assert_eq!(raw.shell.sandbox, ShellSandbox::Unsandboxed);
    // Opening it again returns it unchanged.
    let again = session
        .user_shell(&client, "you@devbox-2", ShellSandbox::Unsandboxed)
        .await
        .unwrap();
    assert_eq!(again.shell_id, raw.shell_id);

    // The user types in it.
    type_into(&env, raw.shell_id.as_str(), b"echo typed > typed.txt\r");
    let typed = env.project().join("typed.txt");
    wait_until("the typed command ran", || typed.exists()).await;

    // The agent's bash doesn't find it, and the agent can't open a shell by its name.
    let err = session
        .run(&client, "c1", bash_op("you@devbox", "echo hi", None))
        .await
        .unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::ShellGone { .. })),
        "{err:?}"
    );
    let err = client
        .call::<methods::ShellOpen>(ShellOpenParams::agent(
            session.workspace,
            "you@devbox".into(),
            None,
        ))
        .await
        .unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::ShellGone { .. })),
        "{err:?}"
    );
    // An agent shell is never unsandboxed.
    let err = client
        .call::<methods::ShellOpen>(ShellOpenParams {
            sandbox: ShellSandbox::Unsandboxed,
            ..ShellOpenParams::agent(session.workspace, "devbox-3".into(), None)
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::InvalidParams)),
        "{err:?}"
    );
    // The agent's own shell still works.
    let (exit, _, _) = session.bash(&client, "c2", "devbox", "true").await;
    assert_eq!(exit, Some(0));

    let shells = listed(&client).await;
    assert_eq!(names(&shells), ["devbox", "you@devbox", "you@devbox-2"]);
    // Every opening is logged with its device.
    let log = env.log();
    assert!(
        log.lines().any(|l| l.contains("user terminal opened")
            && l.contains("unsandboxed")
            && l.contains("Laptop")),
        "{log}"
    );
}

#[tokio::test]
async fn a_user_terminal_ends_on_exit_and_is_not_reopened() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    let mine = session
        .user_shell(&client, "you@devbox", ShellSandbox::Unsandboxed)
        .await
        .unwrap();
    type_exit(&env, mine.shell_id.as_str());
    gone(&client, "you@devbox").await;
    assert!(listed(&client).await.is_empty());
    // A terminal that ended can't be reached any more.
    let err = session
        .run(&client, "c1", bash_op("you@devbox", "true", None))
        .await
        .unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::ShellGone { .. })));
}

#[tokio::test]
async fn user_terminals_end_when_closed_or_with_their_workspace() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    session.shell(&client, "devbox").await;
    let closed = session
        .user_shell(&client, "you@devbox", ShellSandbox::Workspace)
        .await
        .unwrap();
    let other = session
        .user_shell(&client, "you@devbox-2", ShellSandbox::Unsandboxed)
        .await
        .unwrap();
    type_into(&env, other.shell_id.as_str(), b"echo $$ > pid.txt\r");
    let pid_file = env.project().join("pid.txt");
    wait_until("the terminal wrote its pid", || {
        std::fs::read_to_string(&pid_file).is_ok_and(|t| t.ends_with('\n'))
    })
    .await;
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_owned();

    // Closing one (the session was archived, or the user closed its tab).
    client
        .call::<methods::ShellClose>(ShellCloseParams {
            shell_id: closed.shell_id.clone(),
        })
        .await
        .unwrap();
    gone(&client, "you@devbox").await;
    assert_eq!(names(&listed(&client).await), ["devbox", "you@devbox-2"]);

    // Removing the workspace ends the rest.
    client
        .call::<methods::WorkspaceClose>(WorkspaceCloseParams {
            workspace_id: session.workspace,
        })
        .await
        .unwrap();
    assert!(listed(&client).await.is_empty());
    wait_until("the terminal's shell ended", || {
        !std::path::Path::new(&format!("/proc/{pid}")).exists()
    })
    .await;
}

#[tokio::test]
async fn a_restart_keeps_a_live_user_terminal_and_never_respawns_a_dead_one() {
    let mut env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    session.shell(&client, "devbox").await;
    let mine = session
        .user_shell(&client, "you@devbox", ShellSandbox::Unsandboxed)
        .await
        .unwrap();
    type_into(&env, mine.shell_id.as_str(), b"export KEEP=kept\r");
    drop(client);

    // The daemon restarts; the terminal's supervisor survives and is reattached.
    env.kill_daemon();
    env.start_daemon().await;
    let client = env.controller().await;
    let shells = listed(&client).await;
    assert_eq!(names(&shells), ["devbox", "you@devbox"]);
    assert_eq!(shells[1].shell.state, ShellState::Idle);
    assert_eq!(shells[1].shell.owner, ShellOwner::User);
    type_into(&env, mine.shell_id.as_str(), b"echo $KEEP > kept.txt\r");
    let kept = env.project().join("kept.txt");
    wait_until("the reattached terminal ran a command", || {
        std::fs::read_to_string(&kept).is_ok_and(|t| t == "kept\n")
    })
    .await;
    drop(client);

    // The host restarts without the supervisors: the agent's shell is reopened later, the
    // user's terminal is gone for good.
    env.kill_daemon();
    crate::harness::kill_everything_under(&env.state());
    crate::harness::kill_everything_under(&env.project());
    env.start_daemon().await;
    let client = env.controller().await;
    assert_eq!(names(&listed(&client).await), ["devbox"]);
    assert!(!process_in(&env.project()), "nothing was respawned");
    // Opening the name again is a new terminal, never the old one.
    session
        .user_shell(&client, "you@devbox", ShellSandbox::Workspace)
        .await
        .unwrap();
    let fresh = listed(&client).await;
    assert_eq!(fresh[1].shell.sandbox, ShellSandbox::Workspace);
    // Its journal row is gone too: another restart (with its new supervisor) keeps only it.
    drop(client);
    env.kill_daemon();
    env.start_daemon().await;
    let client = env.controller().await;
    assert_eq!(names(&listed(&client).await), ["devbox", "you@devbox"]);
}

/// Types `exit` into a terminal (`pty attach` then fails: the terminal ends under it).
fn type_exit(env: &HostEnv, shell_id: &str) {
    use std::io::Write as _;
    let mut attach = env
        .command()
        .args(["pty", "attach", shell_id, "--state-dir"])
        .arg(env.state())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    attach.stdin.as_mut().unwrap().write_all(b"exit\r").unwrap();
    drop(attach.stdin.take());
    attach.wait().unwrap();
}

/// Whether a process runs in `dir` (shells start in their workspace).
fn process_in(dir: &std::path::Path) -> bool {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .any(|p| std::fs::read_link(p.path().join("cwd")).is_ok_and(|cwd| cwd.starts_with(dir)))
}
