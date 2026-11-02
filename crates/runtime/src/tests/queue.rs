//! The queue: auto-advance, pause on failure and cancel, send-now, editing.

use controlroom_drivers::Capabilities;
use controlroom_drivers::fake::wire::Command;
use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::ErrorData;
use controlroom_protocol::daemon::{
    PromptResult, QueueEditParams, QueueRemoveParams, QueueReorderParams, QueueSendNowParams,
    RequestRespondParams, SendNowResult, SessionUpdateParams,
};
use controlroom_protocol::ids::{OpId, QueueId, SessionId};
use controlroom_protocol::session::{
    RequestKind, RequestState, Response, RunOutcome, SessionState,
};

use super::{Harness, caps, text};

fn queued(result: PromptResult) -> QueueId {
    match result {
        PromptResult::Queued { queue_id } => queue_id,
        other => panic!("expected queued, got {other:?}"),
    }
}

fn send_now(id: SessionId, queue_id: QueueId) -> QueueSendNowParams {
    QueueSendNowParams {
        op_id: OpId::generate(),
        id,
        queue_id,
        interrupt: false,
    }
}

fn cancels(h: &Harness, id: SessionId) -> usize {
    h.host
        .commands(id)
        .iter()
        .filter(|c| matches!(c, Command::Cancel))
        .count()
}

#[tokio::test]
async fn queue_auto_advances_and_pauses_on_failure() {
    let script = FakeScript::default()
        .turn(
            "slow",
            [FakeStep::Text("slow".into()), FakeStep::Gate("slow".into())],
        )
        .turn(
            "error",
            [
                FakeStep::Text("partial".into()),
                FakeStep::Fail("Overloaded".into()),
            ],
        )
        .default_turn([FakeStep::Text("ok".into())]);
    let mut h = Harness::new(script).await;
    let id = h.create().await;

    assert_eq!(h.prompt(id, "slow one").await, PromptResult::Sent);
    queued(h.prompt(id, "second").await);
    queued(h.prompt(id, "third").await);
    h.wait(id, "slow turn", |s| s.preview == "slow" && s.queue.len == 2)
        .await;
    h.host.release(id, "slow");
    h.wait(id, "queue drained", |s| {
        s.queue.len == 0 && s.state == SessionState::Idle
    })
    .await;
    assert_eq!(h.host.prompts(id), ["slow one", "second", "third"]);

    // A failed run pauses the queue; the session shows the failure.
    h.prompt(id, "slow two").await;
    h.prompt(id, "error now").await;
    h.prompt(id, "resumed item").await;
    h.wait(id, "slow turn", |s| s.preview == "slow").await;
    h.host.release(id, "slow");
    let s = h.wait_state(id, SessionState::Failed).await;
    assert!(s.queue.paused);
    assert_eq!(s.queue.len, 1);
    assert_eq!(s.last_outcome, Some(RunOutcome::Failed));
    assert_eq!(s.preview, "Overloaded");
    assert!(!h.queue(id).await[0].sending);
    assert!(!h.host.prompts(id).contains(&"resumed item".to_owned()));

    h.rt.queue_resume(h.op(id)).await.unwrap();
    h.wait(id, "resumed queue drained", |s| {
        s.queue.len == 0 && !s.queue.paused && s.state == SessionState::Idle
    })
    .await;
    assert_eq!(h.host.prompts(id).last().unwrap(), "resumed item");

    // With auto-send off nothing advances after a clean run either; a direct prompt still
    // goes out at once.
    h.rt.update(SessionUpdateParams {
        op_id: OpId::generate(),
        id,
        name: None,
        auto_send: Some(false),
        auto_continue: None,
        auto_continue_budget: None,
    })
    .await
    .unwrap();
    h.prompt(id, "slow three").await;
    queued(h.prompt(id, "waits").await);
    h.wait(id, "slow turn", |s| s.preview == "slow").await;
    h.host.release(id, "slow");
    let s = h
        .wait(id, "idle with a waiting item", |s| {
            s.state == SessionState::Idle && s.queue.len == 1
        })
        .await;
    assert!(!s.queue.auto_send && !s.queue.paused);
    assert!(!h.queue(id).await[0].sending);
    assert_eq!(h.prompt(id, "direct").await, PromptResult::Sent);

    // The P1 flaky-queue bug: the feed never showed an idle session whose queue was about to
    // advance.
    for s in &h.emitted {
        let advancing = s.queue.len > 0 && s.queue.auto_send && !s.queue.paused;
        assert!(
            !(s.state == SessionState::Idle && advancing),
            "idle flicker with a queue: {s:#?}"
        );
    }
}

fn send_now_script() -> FakeScript {
    FakeScript::default()
        .turn(
            "stream",
            [
                FakeStep::Text("partial".into()),
                FakeStep::Gate("never".into()),
            ],
        )
        .turn(
            "tool",
            [
                FakeStep::Tool {
                    title: "make test".into(),
                    output: "all passed".into(),
                    gate: Some("tool".into()),
                },
                FakeStep::Text("tests ran".into()),
            ],
        )
        .turn(
            "ask",
            [
                FakeStep::Request {
                    kind: RequestKind::Permission,
                    title: "deploy".into(),
                },
                FakeStep::Tool {
                    title: "deploy".into(),
                    output: "done".into(),
                    gate: None,
                },
            ],
        )
        .default_turn([FakeStep::Text("next one handled".into())])
}

#[tokio::test]
async fn send_now_during_text_cancels_and_sends_next() {
    let mut h = Harness::new(send_now_script()).await;
    let id = h.create().await;
    h.prompt(id, "stream please").await;
    h.wait(id, "streaming", |s| s.preview == "partial").await;
    let urgent = queued(h.prompt(id, "urgent").await);
    assert_eq!(
        h.rt.send_now(send_now(id, urgent)).await.unwrap(),
        SendNowResult::Interrupting
    );
    let s = h
        .wait(id, "the urgent run", |s| {
            s.preview == "next one handled" && s.state == SessionState::Idle
        })
        .await;
    assert!(!s.queue.paused, "a send-now cancel doesn't pause the queue");
    assert_eq!(h.host.prompts(id), ["stream please", "urgent"]);
    assert_eq!(cancels(&h, id), 1);
}

#[tokio::test]
async fn send_now_during_tool_waits_for_the_tool() {
    let mut h = Harness::new(send_now_script()).await;
    let id = h.create().await;
    h.prompt(id, "run the tool").await;
    h.wait(id, "tool running", |s| s.preview == "make test")
        .await;
    let urgent = queued(h.prompt(id, "urgent").await);
    assert_eq!(
        h.rt.send_now(send_now(id, urgent)).await.unwrap(),
        SendNowResult::Interrupting
    );
    assert!(h.summary(id).queue.send_now_pending, "waiting for the tool");
    assert_eq!(cancels(&h, id), 0);
    h.host.release(id, "tool");
    h.wait(id, "the urgent run", |s| {
        s.preview == "next one handled" && s.state == SessionState::Idle
    })
    .await;
    assert_eq!(h.host.prompts(id), ["run the tool", "urgent"]);
    assert!(!h.summary(id).queue.send_now_pending);

    // "Interrupt now" escalates a send-now that waits for the tool: it cancels at once (the
    // tool's gate is never released). Pressing send-now again without it changes nothing.
    h.prompt(id, "run the tool again").await;
    h.wait(id, "tool running", |s| s.preview == "make test")
        .await;
    let before = cancels(&h, id);
    let now = queued(h.prompt(id, "now!").await);
    for _ in 0..2 {
        assert_eq!(
            h.rt.send_now(send_now(id, now)).await.unwrap(),
            SendNowResult::Interrupting
        );
    }
    assert!(h.summary(id).queue.send_now_pending);
    assert_eq!(cancels(&h, id), before);
    let interrupt = QueueSendNowParams {
        interrupt: true,
        ..send_now(id, now)
    };
    assert_eq!(
        h.rt.send_now(interrupt).await.unwrap(),
        SendNowResult::Interrupting
    );
    h.wait(id, "the interrupting run", |s| {
        s.preview == "next one handled" && s.state == SessionState::Idle
    })
    .await;
    assert_eq!(h.host.prompts(id).len(), 4);
    assert_eq!(cancels(&h, id), before + 1);
}

#[tokio::test]
async fn interrupt_now_cancels_instead_of_steering() {
    let caps = Capabilities {
        steer: true,
        ..caps()
    };
    let mut h = Harness::with(send_now_script(), caps).await;
    let id = h.create().await;
    h.prompt(id, "run the tool").await;
    h.wait(id, "tool running", |s| s.preview == "make test")
        .await;
    let now = queued(h.prompt(id, "stop and do this").await);
    let result =
        h.rt.send_now(QueueSendNowParams {
            interrupt: true,
            ..send_now(id, now)
        })
        .await
        .unwrap();
    assert_eq!(result, SendNowResult::Interrupting);
    h.wait(id, "the new run", |s| {
        s.preview == "next one handled" && s.state == SessionState::Idle
    })
    .await;
    assert_eq!(h.host.prompts(id), ["run the tool", "stop and do this"]);
    assert_eq!(cancels(&h, id), 1);
}

#[tokio::test]
async fn send_now_on_a_stopped_session_is_queued_until_the_agent_is_up() {
    let mut h = Harness::new(send_now_script()).await;
    let id = h.create().await;
    h.prompt(id, "stream please").await;
    h.wait(id, "streaming", |s| s.preview == "partial").await;
    let later = queued(h.prompt(id, "later").await);
    // The agent dies mid-run: the item waits in a paused queue of a stopped session.
    h.host.kill_agent(id);
    let s = h.wait_state(id, SessionState::Interrupted).await;
    assert!(s.queue.paused && s.queue.len == 1);
    assert_eq!(
        h.rt.send_now(send_now(id, later)).await.unwrap(),
        SendNowResult::Queued
    );
    h.wait(id, "the item's run", |s| {
        s.preview == "next one handled" && s.state == SessionState::Idle
    })
    .await;
    assert_eq!(h.host.prompts(id), ["stream please", "later"]);
}

#[tokio::test]
async fn send_now_during_permission_withdraws_the_request() {
    let mut h = Harness::new(send_now_script()).await;
    let id = h.create().await;
    h.prompt(id, "ask first").await;
    let s = h.wait_state(id, SessionState::AwaitingPermission).await;
    let request_id = s.requests[0].id.clone();
    let never = queued(h.prompt(id, "never mind").await);
    assert_eq!(
        h.rt.send_now(send_now(id, never)).await.unwrap(),
        SendNowResult::Interrupting
    );
    h.wait(id, "the next run", |s| {
        s.preview == "next one handled" && s.state == SessionState::Idle
    })
    .await;
    let err =
        h.rt.respond(RequestRespondParams {
            op_id: OpId::generate(),
            id,
            request_id,
            response: Response::Choice {
                option_id: "allow-once".into(),
                reason: None,
            },
            then: None,
            by: None,
        })
        .await
        .unwrap_err();
    let Some(ErrorData::AlreadyResolved { request }) = err.data else {
        panic!("{err:?}");
    };
    assert_eq!(request.state, RequestState::Withdrawn);
    assert_eq!(request.answer, None);
    assert!(
        !h.host
            .commands(id)
            .iter()
            .any(|c| matches!(c, Command::Answer { .. }))
    );
}

#[tokio::test]
async fn send_now_steers_when_the_driver_can() {
    let caps = Capabilities {
        steer: true,
        ..caps()
    };
    let mut h = Harness::with(send_now_script(), caps).await;
    let id = h.create().await;
    h.prompt(id, "stream please").await;
    h.wait(id, "streaming", |s| s.preview == "partial").await;
    let steer = queued(h.prompt(id, "steer this way").await);
    let result = h.rt.send_now(send_now(id, steer)).await.unwrap();
    assert_eq!(result, SendNowResult::Steered);
    h.wait(id, "steered", |s| {
        s.queue.len == 0 && s.preview == "Steered: steer this way"
    })
    .await;
    assert_eq!(cancels(&h, id), 0);
    assert_eq!(h.host.prompts(id), ["stream please"]);
}

#[tokio::test]
async fn cancel_pauses_the_queue() {
    let mut h = Harness::new(send_now_script()).await;
    let id = h.create().await;
    h.prompt(id, "stream please").await;
    h.wait(id, "streaming", |s| s.preview == "partial").await;
    queued(h.prompt(id, "later").await);
    h.rt.cancel(h.op(id)).await.unwrap();
    let s = h
        .wait(id, "cancelled", |s| {
            s.last_outcome == Some(RunOutcome::Cancelled)
        })
        .await;
    assert!(s.queue.paused);
    assert_eq!(s.queue.len, 1);
    assert!(s.unread);
    assert_eq!(s.state, SessionState::Idle);
    assert_eq!(h.host.prompts(id), ["stream please"]);
}

#[tokio::test]
async fn queue_edit_remove_reorder() {
    let mut h = Harness::new(send_now_script()).await;
    let id = h.create().await;
    h.prompt(id, "stream please").await;
    h.wait(id, "streaming", |s| s.preview == "partial").await;
    let a = queued(h.prompt(id, "a").await);
    let b = queued(h.prompt(id, "b").await);
    let c = queued(h.prompt(id, "c").await);
    h.rt.queue_edit(QueueEditParams {
        op_id: OpId::generate(),
        id,
        queue_id: a,
        content: text("a2"),
    })
    .await
    .unwrap();
    h.rt.queue_remove(QueueRemoveParams {
        op_id: OpId::generate(),
        id,
        queue_id: b,
    })
    .await
    .unwrap();
    h.rt.queue_reorder(QueueReorderParams {
        op_id: OpId::generate(),
        id,
        queue_id: c,
        to_index: 0,
    })
    .await
    .unwrap();
    let items: Vec<_> = h.queue(id).await.into_iter().map(|i| i.content).collect();
    assert_eq!(items, [text("c"), text("a2")]);
    let err =
        h.rt.queue_remove(QueueRemoveParams {
            op_id: OpId::generate(),
            id,
            queue_id: b,
        })
        .await
        .unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::NotFound { .. })));
}
