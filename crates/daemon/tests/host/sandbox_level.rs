//! A workspace reopened at another level (`session.set_sandbox_level` on the agent daemon): its
//! profiles are rewritten, the agent's shells restart under the new one (their next command says
//! so), and the user's sandboxed terminals and running jobs keep the sandbox they started with.
//!
//! The workspace runs under a stand-in nono that confines nothing: it runs the command and
//! exports the profile it was given as `CR_TEST_PROFILE`, so a process shows which profile it
//! was started under. Real confinement across a level change is checked in `nono.rs`.

use std::path::{Path, PathBuf};

use controlroom_protocol::host::{
    HostOp, HostResult, ShellListParams, WorkspaceOpenParams, methods,
};
use controlroom_protocol::session::{SandboxLevel, ShellOwner, ShellSandbox, ShellState};

use crate::harness::{
    HostEnv, RpcClient, Session, fifo, release, sandbox, type_into, wait_until, write_executable,
};

/// Runs the command after `--`, with the `--profile` it was given in `CR_TEST_PROFILE`.
const STAND_IN_NONO: &str = r#"#!/bin/sh
[ "$1" = --version ] && { echo 'nono 0.80.0'; exit 0; }
profile=
while [ $# -gt 0 ] && [ "$1" != -- ]; do
  [ "$1" = --profile ] && profile=$2
  shift
done
shift
CR_TEST_PROFILE=$profile exec "$@"
"#;

/// A daemon whose bundled nono is the stand-in.
async fn host_with_stand_in_nono() -> HostEnv {
    let mut env = HostEnv::new();
    let nono = env.dir.path().join("stand-in-nono");
    write_executable(&nono, STAND_IN_NONO);
    env.install(&nono, "0.80.0");
    env.start_daemon().await;
    env
}

async fn reopen(client: &RpcClient, session: &Session, level: SandboxLevel) {
    client
        .call::<methods::WorkspaceOpen>(WorkspaceOpenParams {
            workspace_id: session.workspace,
            session_id: session.id,
            path: "~/project".into(),
            sandbox: sandbox(level),
        })
        .await
        .unwrap();
}

fn profiles(env: &HostEnv, session: &Session) -> [PathBuf; 2] {
    let dir = env
        .state()
        .join("workspaces")
        .join(session.workspace.to_string());
    [dir.join("profile.json"), dir.join("profile-user.json")]
}

/// Runs `command` in the agent shell `devbox`: its output and reset notice.
async fn agent_bash(
    client: &RpcClient,
    session: &Session,
    call: &str,
    command: &str,
) -> (String, Option<String>) {
    let (exit, output, result) = session.bash(client, call, "devbox", command).await;
    assert_eq!(exit, Some(0), "{output}");
    let HostResult::Bash { shell_reset, .. } = result else {
        unreachable!()
    };
    (output.trim().to_owned(), shell_reset)
}

/// What `command` in the user's terminal writes to `file`.
async fn typed(env: &HostEnv, shell_id: &str, command: &str, file: &Path) -> String {
    let _ = std::fs::remove_file(file);
    type_into(
        env,
        shell_id,
        format!("{command} > {}\r", file.display()).as_bytes(),
    );
    wait_until("the typed command ran", || {
        std::fs::read_to_string(file).is_ok_and(|s| s.ends_with('\n'))
    })
    .await;
    std::fs::read_to_string(file).unwrap().trim().to_owned()
}

#[tokio::test]
async fn a_level_change_restarts_the_agents_shells_and_leaves_jobs_and_user_terminals() {
    let env = host_with_stand_in_nono().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Strict).await;
    let [agent_profile, user_profile] = profiles(&env, &session);
    assert!(agent_profile.exists() && user_profile.exists());
    let agent_profile = agent_profile.display().to_string();
    let user_profile_text = user_profile.display().to_string();
    session.shell(&client, "devbox").await;
    let (out, reset) = agent_bash(
        &client,
        &session,
        "c1",
        "export KEEP=1; echo $CR_TEST_PROFILE",
    )
    .await;
    assert_eq!((out.as_str(), reset), (agent_profile.as_str(), None));

    // A sandboxed terminal of the user's and a job, both under the strict profiles.
    let mine = session
        .user_shell(&client, "you@devbox", ShellSandbox::Workspace)
        .await
        .unwrap();
    let user_file = env.dir.path().join("user.txt");
    let seen = typed(
        &env,
        mine.shell_id.as_str(),
        "echo $CR_TEST_PROFILE",
        &user_file,
    )
    .await;
    assert_eq!(seen, user_profile_text);
    let gate = env.dir.path().join("gate");
    fifo(&gate);
    let job_file = env.dir.path().join("job.txt");
    let job = HostOp::JobRun {
        command: format!(
            "cat {} > /dev/null; echo \"$CR_TEST_PROFILE\" > {}",
            gate.display(),
            job_file.display()
        ),
        cwd: None,
        name: Some("long".into()),
        timeout_s: None,
        follow: None,
    };
    session.run(&client, "j1", job).await.unwrap();

    // Off: no profiles; the agent's shell restarts unsandboxed and its next command says so.
    reopen(&client, &session, SandboxLevel::Off).await;
    assert!(!Path::new(&agent_profile).exists() && !user_profile.exists());
    let shells = client
        .call::<methods::ShellList>(ShellListParams { workspace_id: None })
        .await
        .unwrap()
        .shells;
    let agent = shells
        .iter()
        .find(|s| s.shell.owner == ShellOwner::Agent)
        .unwrap();
    assert_eq!(agent.shell.state, ShellState::Idle);
    assert!(agent.shell.reset_pending);
    let (out, reset) = agent_bash(
        &client,
        &session,
        "c2",
        "echo ${KEEP:-lost} $CR_TEST_PROFILE",
    )
    .await;
    assert_eq!(out, "lost");
    assert_eq!(reset.as_deref(), Some("the sandbox level changed"));

    // The user's terminal and the job keep the strict profiles until they end.
    let seen = typed(
        &env,
        mine.shell_id.as_str(),
        "echo $CR_TEST_PROFILE",
        &user_file,
    )
    .await;
    assert_eq!(seen, user_profile_text);
    release(&gate).await;
    wait_until("the job finished", || {
        std::fs::read_to_string(&job_file).is_ok_and(|s| s.ends_with('\n'))
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(&job_file).unwrap().trim(),
        agent_profile
    );

    // The same level again restarts nothing.
    agent_bash(&client, &session, "c3", "export KEEP=2").await;
    reopen(&client, &session, SandboxLevel::Off).await;
    let (out, reset) = agent_bash(&client, &session, "c4", "echo $KEEP").await;
    assert_eq!((out.as_str(), reset), ("2", None));

    // Relaxed: the profiles are back, and so is the shell's sandbox.
    reopen(&client, &session, SandboxLevel::Relaxed).await;
    assert!(Path::new(&agent_profile).exists() && user_profile.exists());
    let (out, reset) = agent_bash(
        &client,
        &session,
        "c5",
        "echo ${KEEP:-lost} $CR_TEST_PROFILE",
    )
    .await;
    assert_eq!(out, format!("lost {agent_profile}"));
    assert_eq!(reset.as_deref(), Some("the sandbox level changed"));
}
