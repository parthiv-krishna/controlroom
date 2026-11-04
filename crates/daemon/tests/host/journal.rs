//! The journal: attach after a disconnect, exactly once across resends and daemon restarts.

use controlroom_protocol::host::{HostResult, JournalAckParams, ToolCallRef, methods};
use controlroom_protocol::ids::CallId;
use controlroom_protocol::session::SandboxLevel;
use controlroom_protocol::{ErrorData, Resource};

use crate::harness::{HostEnv, Session, bash_op, fifo, release, wait_until};

fn lines(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path).map_or(0, |s| s.lines().count())
}

#[tokio::test]
async fn a_call_outlives_its_connection_and_never_runs_twice() {
    let env = HostEnv::start().await;
    let first = env.controller().await;
    let session = Session::open(&env, &first, SandboxLevel::Off).await;
    session.shell(&first, "s").await;
    let gate = env.dir.path().join("gate");
    fifo(&gate);
    let ran = env.dir.path().join("ran.txt");
    let command = format!(
        "echo x >> {}; cat {} > /dev/null; echo released",
        ran.display(),
        gate.display()
    );
    let params = session.run_params("c1", bash_op("s", &command, None));
    drop(first.send::<methods::ToolRun>(params.clone()).await);
    wait_until("the command ran", || lines(&ran) == 1).await;
    drop(first);

    // A new connection (the controller reconnected) resends the call: it attaches.
    let second = env.controller().await;
    let resent = second.send::<methods::ToolRun>(params.clone()).await;
    let attached = second
        .send::<methods::ToolAttach>(ToolCallRef {
            call_id: CallId::new("c1"),
        })
        .await;
    release(&gate).await;
    for outcome in [resent.await, attached.await] {
        let result: HostResult = serde_json::from_value(outcome.unwrap().unwrap()).unwrap();
        assert!(
            matches!(&result, HostResult::Bash { output, exit_code: Some(0), .. } if output == "released"),
            "{result:?}"
        );
    }
    // Once more: the journaled result, no rerun.
    let again = second
        .call::<methods::ToolRun>(params.clone())
        .await
        .unwrap();
    assert!(matches!(again, HostResult::Bash { ref output, .. } if output == "released"));
    assert_eq!(lines(&ran), 1);

    // The same id for something else is refused.
    let mut other = params.clone();
    other.op = bash_op("s", "true", None);
    let err = second.call::<methods::ToolRun>(other).await.unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::InvalidParams)),
        "{err:?}"
    );

    // Acked results are gone.
    second
        .call::<methods::JournalAck>(JournalAckParams {
            call_ids: vec![CallId::new("c1")],
        })
        .await
        .unwrap();
    let err = second
        .call::<methods::ToolAttach>(ToolCallRef {
            call_id: CallId::new("c1"),
        })
        .await
        .unwrap_err();
    assert!(matches!(
        err.data,
        Some(ErrorData::NotFound {
            resource: Resource::Call,
            ..
        })
    ));
}

#[tokio::test]
async fn a_daemon_restart_mid_command_recovers_the_result_from_the_surviving_shell() {
    let mut env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    session.shell(&client, "s").await;
    let gate = env.dir.path().join("gate");
    fifo(&gate);
    let ran = env.dir.path().join("ran.txt");
    let command = format!(
        "cd sub; echo x >> {}; echo before; cat {} > /dev/null; echo after-$((1+1))",
        ran.display(),
        gate.display()
    );
    let params = session.run_params("c1", bash_op("s", &command, None));
    drop(client.send::<methods::ToolRun>(params.clone()).await);
    wait_until("the command ran", || lines(&ran) == 1).await;
    let old_epoch = {
        let (_, hello) = env
            .connect(controlroom_protocol::agentd::ClientRole::Debug)
            .await;
        hello.epoch
    };
    drop(client);
    env.kill_daemon();
    env.start_daemon().await;
    let client = env.controller().await;
    let (_, hello) = env
        .connect(controlroom_protocol::agentd::ClientRole::Debug)
        .await;
    assert_ne!(hello.epoch, old_epoch, "a restart is a new epoch");

    let resent = client.send::<methods::ToolRun>(params.clone()).await;
    release(&gate).await;
    let result: HostResult = serde_json::from_value(resent.await.unwrap().unwrap()).unwrap();
    let HostResult::Bash {
        output,
        exit_code,
        cwd,
        ..
    } = result
    else {
        panic!("{result:?}")
    };
    assert_eq!(output, "before\nafter-2");
    assert_eq!(exit_code, Some(0));
    assert!(cwd.as_str().ends_with("/project/sub"));
    assert_eq!(lines(&ran), 1);
    // The shell kept its state.
    let (_, output, _) = session.bash(&client, "c2", "s", "pwd").await;
    assert!(output.ends_with("/project/sub"));
}

#[tokio::test]
async fn a_command_whose_shell_died_with_the_daemon_is_lost_and_the_shell_reopens() {
    let mut env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    session.shell(&client, "s").await;
    session.bash(&client, "c0", "s", "cd sub").await;
    let gate = env.dir.path().join("gate");
    fifo(&gate);
    let ran = env.dir.path().join("ran.txt");
    let command = format!(
        "echo x >> {}; cat {} > /dev/null",
        ran.display(),
        gate.display()
    );
    let params = session.run_params("c1", bash_op("s", &command, None));
    drop(client.send::<methods::ToolRun>(params.clone()).await);
    wait_until("the command ran", || lines(&ran) == 1).await;
    drop(client);
    // The host "reboots": the daemon and every shell process die.
    env.kill_daemon();
    crate::harness::kill_everything_under(&env.state());
    crate::harness::kill_everything_under(&env.project());
    env.start_daemon().await;
    let client = env.controller().await;
    let err = client
        .call::<methods::ToolRun>(params.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::Lost { may_have_run: true })),
        "{err:?}"
    );
    assert_eq!(lines(&ran), 1);
    let (_, output, result) = session.bash(&client, "c2", "s", "pwd").await;
    assert!(output.ends_with("/project/sub"), "{output}");
    let HostResult::Bash { shell_reset, .. } = result else {
        unreachable!()
    };
    assert!(shell_reset.is_some());
}

#[tokio::test]
async fn a_write_interrupted_by_a_restart_is_resolved_by_the_files_hash() {
    let mut env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    drop(client);
    env.kill_daemon();
    // What a daemon that died mid-write leaves: a running row with its intent. One write
    // reached the disk, the other didn't.
    let target = env.project().join("done.txt");
    std::fs::write(&target, "new content").unwrap();
    let sha = |s: &str| {
        use sha2::Digest as _;
        sha2::Sha256::digest(s.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let db = rusqlite::Connection::open(env.state().join("host.sqlite")).unwrap();
    for (call, path, content) in [
        ("w-applied", target.clone(), "new content"),
        ("w-missing", env.project().join("never.txt"), "other"),
    ] {
        let intent = serde_json::json!({
            "op": "write", "path": path, "sha256": sha(content), "bytes": content.len()
        });
        db.execute(
            "INSERT INTO journal (call_id, workspace_id, op, args_hash, state, intent, started_at)
             VALUES (?1, ?2, 'write', 'x', 'running', ?3, 0)",
            rusqlite::params![call, session.workspace.to_string(), intent.to_string()],
        )
        .unwrap();
    }
    drop(db);
    env.start_daemon().await;
    let client = env.controller().await;
    let applied = client
        .call::<methods::ToolAttach>(ToolCallRef {
            call_id: CallId::new("w-applied"),
        })
        .await
        .unwrap();
    assert!(
        matches!(applied, HostResult::Write { bytes: 11, .. }),
        "{applied:?}"
    );
    let missing = client
        .call::<methods::ToolAttach>(ToolCallRef {
            call_id: CallId::new("w-missing"),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(
            missing.data,
            Some(ErrorData::Lost {
                may_have_run: false
            })
        ),
        "{missing:?}"
    );
}
