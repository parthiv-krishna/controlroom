//! The agent's host calls through the fake controller: routing, link drops within the grace
//! period (the call pauses and runs once), outages beyond it (`connection_lost`, then the late
//! result is injected), a host daemon restart (`lost`, and the shell is reopened), output and
//! waits for terminal input, cancellation, journal acks, and late calls across an agent daemon
//! restart.

use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::agent::{HostCallParams, HostCallResult, HostCancelParams};
use controlroom_protocol::host::{HostOp, HostResult};
use controlroom_protocol::ids::{CallId, RemotePath, SessionId};
use controlroom_protocol::session::{RequestKind, RequestState, SessionState};
use controlroom_protocol::timeline::{InjectOrigin, ItemBody, RunTrigger, ToolStatus};
use controlroom_protocol::{ErrorData, Resource, RpcError};
use tokio::task::JoinHandle;

use super::Harness;
use crate::AgentEvent;

fn bash(call: &str, command: &str) -> HostCallParams {
    HostCallParams {
        call_id: CallId::new(call),
        machine: "devbox".into(),
        op: HostOp::Bash {
            shell: "devbox".into(),
            command: command.into(),
            timeout_s: None,
        },
    }
}

/// Starts a host call in the background.
fn spawn_call(
    h: &Harness,
    id: SessionId,
    params: HostCallParams,
) -> JoinHandle<Result<HostCallResult, RpcError>> {
    let rt = h.rt.clone();
    tokio::spawn(async move { rt.host_call(id, params).await })
}

/// Waits until the machine started `command`.
async fn wait_ran(h: &Harness, command: &str) {
    let devbox = h.devbox;
    let command = command.to_owned();
    h.machines
        .wait("the command to start", move |m| {
            m.ran(devbox).contains(&command)
        })
        .await;
}

fn bash_output(result: &HostCallResult) -> &str {
    match &result.result {
        HostResult::Bash { output, .. } => output,
        other => panic!("not a bash result: {other:?}"),
    }
}

#[tokio::test]
async fn calls_are_routed_to_the_sessions_workspaces() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let write = HostCallParams {
        call_id: CallId::new("w1"),
        machine: "devbox".into(),
        op: HostOp::Write {
            path: RemotePath::new("notes.md"),
            content: "hello".into(),
            encoding: Default::default(),
            expect_sha256: None,
        },
    };
    let written = h.rt.host_call(id, write).await.unwrap();
    assert!(matches!(written.result, HostResult::Write { bytes: 5, .. }));
    assert_eq!(
        h.machines
            .file(h.devbox, "/home/alice/src/app/notes.md")
            .as_deref(),
        Some("hello")
    );
    let ran = h.rt.host_call(id, bash("b1", "make")).await.unwrap();
    assert_eq!((bash_output(&ran), ran.link_drops), ("ok: make\n", 0));
    // The default shell was opened before its first command.
    assert!(
        h.machines
            .methods(h.devbox)
            .contains(&"shell.open".to_owned())
    );

    let elsewhere = HostCallParams {
        machine: "buildbox".into(),
        ..bash("b2", "x")
    };
    let err = h.rt.host_call(id, elsewhere).await.unwrap_err();
    assert!(matches!(
        err.data,
        Some(ErrorData::NotFound {
            resource: Resource::Workspace,
            ..
        })
    ));
    let no_shell = HostCallParams {
        op: HostOp::Bash {
            shell: "nope".into(),
            command: "x".into(),
            timeout_s: None,
        },
        ..bash("b3", "x")
    };
    let err = h.rt.host_call(id, no_shell).await.unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::ShellGone { .. })));
    let job = HostCallParams {
        op: HostOp::JobStatus { job_id: None },
        ..bash("b4", "x")
    };
    let err = h.rt.host_call(id, job).await.unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::InvalidParams)));
    // A resend of a finished call returns the journaled result; nothing runs twice.
    let again = h.rt.host_call(id, bash("b1", "make")).await.unwrap();
    assert_eq!(bash_output(&again), "ok: make\n");
    assert_eq!(h.machines.ran(h.devbox), ["make"]);
    h.wait_state(id, SessionState::Idle).await;
}

#[tokio::test]
async fn a_link_drop_within_the_grace_pauses_the_call_and_it_runs_once() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let call = spawn_call(&h, id, bash("c1", "block g1"));
    wait_ran(&h, "block g1").await;
    h.machines.drop_link(h.devbox);
    assert_eq!(h.summary(id).live_work.host_calls, 1);
    h.clock.advance(30_000);
    h.rt.tick();
    h.machines.release("g1");
    h.machines.reconnect(h.devbox);
    let result = call.await.unwrap().unwrap();
    assert_eq!(bash_output(&result), "ran g1\n");
    assert_eq!(result.link_drops, 1);
    assert_eq!(h.machines.ran(h.devbox), ["block g1"], "it ran once");
    h.wait(id, "no waiting calls", |s| s.live_work.host_calls == 0)
        .await;
}

#[tokio::test]
async fn past_the_grace_the_call_fails_and_its_late_result_is_injected() {
    let mut h =
        Harness::new(FakeScript::default().default_turn([FakeStep::Text("noted".into())])).await;
    let id = h.create().await;
    let devbox = h.devbox;
    let call = spawn_call(&h, id, bash("c2", "block g2"));
    wait_ran(&h, "block g2").await;
    h.machines.drop_link(devbox);
    h.clock.advance(600_000);
    h.rt.tick();
    let err = call.await.unwrap().unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::ConnectionLost { machine_id, .. }) if machine_id == devbox),
        "{err:?}"
    );
    assert_eq!(
        h.summary(id).live_work.host_calls,
        1,
        "the late result is awaited"
    );
    // A resend after the failure fails the same way, without running anything.
    let err =
        h.rt.host_call(id, bash("c2", "block g2"))
            .await
            .unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::ConnectionLost { .. })));

    h.machines.reconnect(devbox);
    h.machines.release("g2");
    h.host
        .wait("the late result", |host| {
            host.prompts(id).iter().any(|p| p.contains("ran g2"))
        })
        .await;
    let s = h.wait_state(id, SessionState::Idle).await;
    assert_eq!(s.live_work.host_calls, 0);
    let prompt = h.host.prompts(id).pop().unwrap();
    assert!(
        prompt.contains("Your earlier call c2 (bash `block g2`)")
            && prompt.contains("it ran once on devbox and ended with exit 0"),
        "{prompt}"
    );
    assert_eq!(
        h.run_triggers(id).await,
        [RunTrigger::Inject {
            origin: InjectOrigin::ToolLate,
            label: "late result: bash `block g2`".into()
        }]
    );
    assert_eq!(h.machines.ran(devbox), ["block g2"]);
}

#[tokio::test]
async fn a_late_call_survives_an_agent_daemon_restart() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    let call = spawn_call(&h, id, bash("c3", "block g3"));
    wait_ran(&h, "block g3").await;
    h.machines.drop_link(devbox);
    h.clock.advance(600_000);
    h.rt.tick();
    assert!(call.await.unwrap().is_err());
    let mut h = h.restart().await;
    h.machines.reconnect(devbox);
    h.machines.release("g3");
    h.host
        .wait("the late result", |host| {
            host.prompts(id).iter().any(|p| p.contains("ran g3"))
        })
        .await;
    h.wait_state(id, SessionState::Idle).await;
    assert_eq!(h.machines.ran(devbox), ["block g3"], "it ran once");
    let h = h.restart().await;
    h.clock.advance(1000);
    h.rt.tick();
    let late = h
        .host
        .prompts(id)
        .iter()
        .filter(|p| p.contains("ran g3"))
        .count();
    assert_eq!(late, 1, "injected once");
}

#[tokio::test]
async fn a_call_that_never_reached_the_host_is_reported_as_not_run() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    h.machines.drop_link(devbox);
    let call = spawn_call(&h, id, bash("c4", "make"));
    h.wait(id, "the waiting call", |s| s.live_work.host_calls == 1)
        .await;
    h.clock.advance(600_000);
    h.rt.tick();
    assert!(call.await.unwrap().is_err());
    h.machines.reconnect(devbox);
    h.host
        .wait("the late notice", |host| {
            host.prompts(id)
                .iter()
                .any(|p| p.contains("never reached devbox"))
        })
        .await;
    assert!(h.machines.ran(devbox).is_empty(), "withdrawn, never run");
}

#[tokio::test]
async fn a_host_restart_loses_the_running_call_and_reopens_the_shell() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    h.rt.host_call(id, bash("c5", "pwd")).await.unwrap();
    let call = spawn_call(&h, id, bash("c6", "block g6"));
    wait_ran(&h, "block g6").await;
    h.machines.restart(devbox);
    let err = call.await.unwrap().unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::Lost { may_have_run: true })),
        "{err:?}"
    );
    h.rt.host_call(id, bash("c7", "pwd")).await.unwrap();
    let opens = h
        .machines
        .methods(devbox)
        .iter()
        .filter(|m| *m == "shell.open")
        .count();
    assert!(opens >= 2, "reopened in the new epoch ({opens})");
    h.machines.release("g6");
}

#[tokio::test]
async fn output_and_waits_for_input_reach_the_extension_and_the_user() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    let mut events = h.rt.agent_events(id).await.unwrap();
    let call = spawn_call(&h, id, bash("c8", "block g8"));
    wait_ran(&h, "block g8").await;
    h.machines.output(devbox, "c8", "Password: ");
    h.machines.waiting_input(devbox, "c8", true);
    assert!(
        matches!(events.recv().await.unwrap(), AgentEvent::Output(p) if p.text == "Password: ")
    );
    assert!(matches!(events.recv().await.unwrap(), AgentEvent::WaitingInput(p) if p.waiting));
    let s = h.wait_state(id, SessionState::AwaitingPermission).await;
    let request = &s.requests[0];
    assert_eq!(request.kind, RequestKind::TerminalInput);
    assert!(request.blocking);
    h.machines.waiting_input(devbox, "c8", false);
    let s = h.wait_state(id, SessionState::Idle).await;
    assert!(s.requests.is_empty());
    h.rt.host_cancel(
        id,
        HostCancelParams {
            call_id: CallId::new("c8"),
        },
    )
    .await
    .unwrap();
    h.machines
        .wait("the cancel", |m| {
            m.methods(devbox).contains(&"tool.cancel".to_owned())
        })
        .await;
    h.machines.release("g8");
    call.await.unwrap().unwrap();
    let items = h.timeline(id).await;
    let input = items.iter().find_map(|i| match &i.body {
        ItemBody::Request { request } if request.kind == RequestKind::TerminalInput => {
            Some(request.state)
        }
        _ => None,
    });
    assert_eq!(input, Some(RequestState::Withdrawn));
}

#[tokio::test]
async fn results_are_released_once_pi_logged_the_tool_call() {
    let script = FakeScript::default().turn(
        "go",
        [FakeStep::Tool {
            title: "remote".into(),
            output: "done".into(),
            gate: Some("tool".into()),
        }],
    );
    let mut h = Harness::new(script).await;
    let id = h.create().await;
    h.prompt(id, "go").await;
    let tool_id = loop {
        h.wait(id, "the tool", |s| s.preview == "remote").await;
        let found = h.timeline(id).await.into_iter().find_map(|i| match i.body {
            ItemBody::ToolCall {
                tool_call_id,
                status: ToolStatus::Running,
                ..
            } => Some(tool_call_id),
            _ => None,
        });
        if let Some(found) = found {
            break found;
        }
    };
    let step = format!("{tool_id}/1");
    h.rt.host_call(id, bash(&step, "ls")).await.unwrap();
    h.clock.advance(1000);
    h.rt.tick();
    assert!(
        h.machines.acked_calls(h.devbox).is_empty(),
        "not before pi logged it"
    );
    h.host.release(id, "tool");
    h.wait_state(id, SessionState::Idle).await;
    h.rt.tick();
    let devbox = h.devbox;
    h.machines
        .wait("the ack", move |m| {
            m.acked_calls(devbox) == [CallId::new(step.clone())]
        })
        .await;
}

#[tokio::test]
async fn the_grace_period_is_the_machines() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    h.machines.set_grace(devbox, 60);
    h.machines.drop_link(devbox);
    let call = spawn_call(&h, id, bash("c9", "make"));
    h.wait(id, "the waiting call", |s| s.live_work.host_calls == 1)
        .await;
    h.clock.advance(59_000);
    h.rt.tick();
    assert!(!call.is_finished());
    h.clock.advance(1_000);
    h.rt.tick();
    let err = call.await.unwrap().unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::ConnectionLost { .. })));
}
