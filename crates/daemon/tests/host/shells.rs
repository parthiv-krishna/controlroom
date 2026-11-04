//! Named shells: state that persists, independence, exit and reopen, interrupt, timeout, and
//! waiting for input.

use controlroom_protocol::ErrorData;
use controlroom_protocol::host::{HostResult, ShellListParams, ToolCallRef, methods};
use controlroom_protocol::ids::CallId;
use controlroom_protocol::session::{SandboxLevel, ShellState};

use crate::harness::{HostEnv, Session, bash_op};

#[tokio::test]
async fn shells_keep_cwd_env_and_functions_and_are_independent() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    session.shell(&client, "devbox").await;
    session.shell(&client, "devbox-2").await;

    let (exit, _, _) = session
        .bash(
            &client,
            "c1",
            "devbox",
            "cd sub && export GREETING=hi && greet() { echo \"$GREETING $1\"; }",
        )
        .await;
    assert_eq!(exit, Some(0));
    let (exit, output, result) = session
        .bash(&client, "c2", "devbox", "pwd; greet there")
        .await;
    assert_eq!(exit, Some(0));
    let sub = env.project().join("sub");
    assert_eq!(output, format!("{}\nhi there", sub.display()));
    let HostResult::Bash {
        cwd,
        shell_reset,
        log_path,
        ..
    } = result
    else {
        unreachable!()
    };
    assert_eq!(cwd.as_str(), sub.to_str().unwrap());
    assert_eq!(shell_reset, None);
    let log = std::fs::read_to_string(log_path.as_str()).unwrap();
    assert!(log.contains("hi there"), "{log:?}");

    // The second shell has none of it.
    let (_, output, _) = session
        .bash(&client, "c3", "devbox-2", "echo ${GREETING-unset}; pwd")
        .await;
    assert_eq!(output, format!("unset\n{}", env.project().display()));

    // A failing command is a result, not an error.
    let (exit, output, _) = session
        .bash(&client, "c4", "devbox", "echo oops >&2; false")
        .await;
    assert_eq!((exit, output.as_str()), (Some(1), "oops"));

    // The shells are listed with their cwd.
    let list = client
        .call::<methods::ShellList>(ShellListParams {
            workspace_id: Some(session.workspace),
        })
        .await
        .unwrap();
    let cwds: Vec<_> = list
        .shells
        .iter()
        .map(|s| {
            (
                s.shell.name.as_str(),
                s.shell.cwd.as_str().to_owned(),
                s.shell.state.clone(),
            )
        })
        .collect();
    assert_eq!(
        cwds,
        [
            (
                "devbox",
                sub.to_string_lossy().into_owned(),
                ShellState::Idle
            ),
            (
                "devbox-2",
                env.project().to_string_lossy().into_owned(),
                ShellState::Idle
            ),
        ]
    );
}

#[tokio::test]
async fn exit_reopens_the_shell_in_its_last_cwd_with_a_reset_notice() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    session.shell(&client, "s").await;
    session
        .bash(&client, "c1", "s", "cd sub; export KEEP=1")
        .await;
    let (exit, _, _) = session.bash(&client, "c2", "s", "exit 3").await;
    assert_eq!(exit, Some(3));
    let (exit, output, result) = session
        .bash(&client, "c3", "s", "echo ${KEEP-lost}; pwd")
        .await;
    assert_eq!(exit, Some(0));
    assert_eq!(
        output,
        format!("lost\n{}", env.project().join("sub").display())
    );
    let HostResult::Bash { shell_reset, .. } = result else {
        unreachable!()
    };
    assert!(shell_reset.is_some_and(|r| r.contains("status 3")));
    // Only once.
    let (_, _, result) = session.bash(&client, "c4", "s", "true").await;
    assert!(matches!(
        result,
        HostResult::Bash {
            shell_reset: None,
            ..
        }
    ));

    // A shell that was never opened.
    let err = session
        .run(&client, "c5", bash_op("nope", "true", None))
        .await
        .unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::ShellGone { .. })),
        "{err:?}"
    );
}

#[tokio::test]
async fn cancel_interrupts_the_command_and_the_shell_survives() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    session.shell(&client, "s").await;
    session.bash(&client, "c1", "s", "export KEEP=yes").await;

    let running = client
        .send::<methods::ToolRun>(session.run_params(
            "slow",
            bash_op("s", "echo started; sleep 1000; echo after", None),
        ))
        .await;
    // Wait until sleep is the terminal's foreground job.
    loop {
        let changed = client.note("shell.changed").await;
        if changed["shell"]["foreground"] == "sleep" {
            break;
        }
    }
    let cancelled = client
        .call::<methods::ToolCancel>(ToolCallRef {
            call_id: CallId::new("slow"),
        })
        .await
        .unwrap();
    assert!(cancelled.cancelled);
    let result: HostResult = serde_json::from_value(running.await.unwrap().unwrap()).unwrap();
    let HostResult::Bash {
        exit_code,
        cancelled,
        output,
        ..
    } = result
    else {
        panic!("{result:?}")
    };
    assert!(cancelled);
    assert_eq!(exit_code, Some(130));
    // The terminal echoes the interrupt as `^C`.
    assert!(
        output.starts_with("started") && !output.contains("after"),
        "{output:?}"
    );

    let (_, output, _) = session.bash(&client, "c2", "s", "echo $KEEP").await;
    assert_eq!(output, "yes");
    // Cancelling a finished call does nothing.
    let again = client
        .call::<methods::ToolCancel>(ToolCallRef {
            call_id: CallId::new("slow"),
        })
        .await
        .unwrap();
    assert!(!again.cancelled);
}

#[tokio::test]
async fn timeouts_hold_on_the_host() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    session.shell(&client, "s").await;
    let result = session
        .run(&client, "t", bash_op("s", "sleep 1000", Some(1)))
        .await
        .unwrap();
    let HostResult::Bash {
        timed_out,
        exit_code,
        ..
    } = result
    else {
        panic!()
    };
    assert!(timed_out);
    assert_eq!(exit_code, Some(130));
}

#[tokio::test]
async fn a_command_waiting_for_input_is_flagged_and_the_user_answers_in_the_terminal() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    session.shell(&client, "s").await;
    let running = client
        .send::<methods::ToolRun>(session.run_params(
            "ask",
            bash_op("s", "read -r -p 'name? ' answer; echo got:$answer", None),
        ))
        .await;
    let waiting = client.note("tool.waiting_input").await;
    assert_eq!(waiting["call_id"], "ask");
    assert_eq!(waiting["waiting"], true);

    // The user types in the terminal pane.
    let shell_id = format!("{}/s", session.id);
    let mut attach = env
        .command()
        .args(["pty", "attach", &shell_id, "--state-dir"])
        .arg(env.state())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    {
        use std::io::Write as _;
        let stdin = attach.stdin.as_mut().unwrap();
        stdin.write_all(b"alice\n").unwrap();
    }
    let result: HostResult = serde_json::from_value(running.await.unwrap().unwrap()).unwrap();
    drop(attach.stdin.take());
    let _ = attach.kill();
    let _ = attach.wait();
    let HostResult::Bash {
        waited_for_input,
        output,
        ..
    } = result
    else {
        panic!()
    };
    assert!(waited_for_input);
    assert!(output.ends_with("got:alice"), "{output:?}");
}
