//! Recovery: a killed daemon loses nothing, dead agents and supervisors, reboots, in-doubt
//! writes, failed commits.

use controlroom_drivers::fake::wire::Command;
use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::ErrorData;
use controlroom_protocol::agentd::{
    IfBusy, PromptResult, RequestRespondParams, SessionPromptParams,
};
use controlroom_protocol::ids::{OpId, Seq, SessionId};
use controlroom_protocol::session::{
    PromptOrigin, RequestKind, Response, RunOutcome, SessionState, StartedBy,
};
use controlroom_transcript::Body;

use super::{Harness, text};

fn allow(id: SessionId, request_id: controlroom_protocol::ids::RequestId) -> RequestRespondParams {
    RequestRespondParams {
        op_id: OpId::generate(),
        id,
        request_id,
        response: Response::Choice {
            option_id: "allow-once".into(),
            reason: None,
        },
        then: None,
        by: None,
    }
}

fn count(h: &Harness, id: SessionId, pred: impl Fn(&Command) -> bool) -> usize {
    h.host.commands(id).iter().filter(|c| pred(c)).count()
}

fn script() -> FakeScript {
    FakeScript::default()
        .turn(
            "ask",
            [
                FakeStep::Request {
                    kind: RequestKind::Permission,
                    title: "deploy".into(),
                },
                FakeStep::Tool {
                    title: "deploy".into(),
                    output: "deployed".into(),
                    gate: None,
                },
                FakeStep::Text("deployed fine".into()),
            ],
        )
        .turn(
            "slow",
            [
                FakeStep::Text("slow".into()),
                FakeStep::Gate("slow".into()),
                FakeStep::Text(" end of slow".into()),
            ],
        )
        .default_turn([FakeStep::Text("queued reply".into())])
}

#[tokio::test]
async fn daemon_killed_mid_turn_loses_nothing() {
    let mut h = Harness::new(script()).await;
    let id = h.create().await;

    // 1. A pending request and the queue survive the daemon.
    h.prompt(id, "ask first").await;
    let s = h.wait_state(id, SessionState::AwaitingPermission).await;
    let request_id = s.requests[0].id.clone();
    h.prompt(id, "after the deploy").await;
    let mut h = h.restart().await;
    let s = h.wait_state(id, SessionState::AwaitingPermission).await;
    assert_eq!(
        s.requests[0].id, request_id,
        "request ids are deterministic"
    );
    assert_eq!(s.queue.len, 1);
    h.rt.respond(allow(id, request_id)).await.unwrap();
    h.wait(id, "queue drained", |s| {
        s.queue.len == 0 && s.state == SessionState::Idle && s.preview == "queued reply"
    })
    .await;
    assert_eq!(h.host.prompts(id), ["ask first", "after the deploy"]);

    // 2. The run finishes while the daemon is down; the queue advances after the restart.
    h.prompt(id, "slow turn").await;
    h.prompt(id, "queued while down").await;
    h.wait(id, "streaming", |s| s.preview == "slow").await;
    let mut h = h.restart_after(|host| host.release(id, "slow")).await;
    h.wait(id, "queued item sent", |s| {
        s.queue.len == 0 && s.state == SessionState::Idle && s.preview == "queued reply"
    })
    .await;
    assert_eq!(
        h.host.prompts(id),
        [
            "ask first",
            "after the deploy",
            "slow turn",
            "queued while down"
        ]
    );
    // Nothing was answered twice, and the agent was asked for its state once.
    assert_eq!(count(&h, id, |c| matches!(c, Command::Answer { .. })), 1);
    assert_eq!(count(&h, id, |c| matches!(c, Command::State)), 1);
}

#[tokio::test]
async fn agent_crash_mid_turn_is_interrupted() {
    let mut h = Harness::new(FakeScript::default().default_turn([
        FakeStep::Text("about to crash".into()),
        FakeStep::Gate("crash".into()),
        FakeStep::Exit(101),
    ]))
    .await;
    let id = h.create().await;
    h.prompt(id, "go").await;
    h.wait(id, "text", |s| s.preview == "about to crash").await;
    assert!(matches!(
        h.prompt(id, "queued").await,
        PromptResult::Queued { .. }
    ));
    h.host.release(id, "crash");
    let s = h.wait_state(id, SessionState::Interrupted).await;
    let exit = h
        .host
        .transcript(id)
        .iter()
        .find(|r| matches!(r.body, Body::Exit(_)))
        .unwrap()
        .seq;
    assert_eq!(s.last_turn_end_seq, Seq(exit));
    assert!(s.unread);
    assert_eq!(s.preview, "the agent exited with code 101");
    assert!(s.queue.paused);
    assert_eq!(s.last_outcome, None);
}

#[tokio::test]
async fn supervisor_killed_mid_turn_is_interrupted() {
    let mut h = Harness::new(script()).await;
    let id = h.create().await;
    h.prompt(id, "slow").await;
    h.wait(id, "streaming", |s| s.preview == "slow").await;
    h.host.kill_supervisor(id);
    let s = h.wait_state(id, SessionState::Interrupted).await;
    // No exit record: the run ends at the last record the supervisor logged.
    let last = h.host.transcript(id).last().unwrap().seq;
    assert_eq!(s.last_turn_end_seq, Seq(last));
    assert!(s.unread);
}

/// The agent got a prompt, but nothing it answered was logged before it went away: the
/// prompt counts as delivered (it is never sent twice), so the run it was starting is
/// interrupted instead of the session looking dormant with the prompt gone. Both without an
/// exit record (the supervisor died) and with one (the agent died), also after a restart.
#[tokio::test]
async fn a_run_cut_off_before_it_started_is_interrupted() {
    let mut h = Harness::new(script()).await;
    let gone = h.create().await;
    let crashed = h.create().await;
    for id in [gone, crashed] {
        h.host.lose_output(id);
        h.prompt(id, "slow").await;
        h.host
            .wait("the prompt logged", |host| host.prompts(id) == ["slow"])
            .await;
    }
    h.host.kill_supervisor(gone);
    h.host.kill_agent(crashed);
    for id in [gone, crashed] {
        let s = h.wait_state(id, SessionState::Interrupted).await;
        assert_eq!(s.queue.len, 0, "the prompt was delivered");
        assert!(s.unread);
    }
    let h = h.restart().await;
    for id in [gone, crashed] {
        assert_eq!(h.summary(id).state, SessionState::Interrupted);
        assert_eq!(h.host.prompts(id), ["slow"]);
    }
}

#[tokio::test]
async fn reboot_leaves_interrupted_or_dormant_sessions() {
    let mut h = Harness::new(script()).await;
    let busy = h.create().await;
    let idle = h.create().await;
    h.prompt(busy, "slow").await;
    h.wait(busy, "streaming", |s| s.preview == "slow").await;
    let h = h
        .restart_after(|host| {
            host.kill_supervisor(busy);
            host.kill_supervisor(idle);
        })
        .await;
    assert_eq!(h.summary(busy).state, SessionState::Interrupted);
    assert_eq!(h.summary(idle).state, SessionState::Dormant);
    // A second restart finds the same and changes nothing.
    let end = h.summary(busy).last_turn_end_seq;
    let h = h.restart().await;
    assert_eq!(h.summary(busy).state, SessionState::Interrupted);
    assert_eq!(h.summary(busy).last_turn_end_seq, end);
}

#[tokio::test]
async fn dormant_session_resumes_lazily_on_prompt() {
    let mut h =
        Harness::new(FakeScript::default().default_turn([FakeStep::Text("reply".into())])).await;
    let id = h.create().await;
    h.prompt(id, "remember walnut").await;
    h.wait(id, "run end", |s| {
        s.last_outcome == Some(RunOutcome::Completed)
    })
    .await;
    h.host.kill_agent(id);
    let s = h.wait_state(id, SessionState::Dormant).await;
    assert!(s.resumable);

    assert!(matches!(
        h.prompt(id, "what was it?").await,
        PromptResult::Queued { .. }
    ));
    h.wait(id, "second run", |s| {
        s.queue.len == 0 && s.state == SessionState::Idle && s.last_turn_end_seq.0 > 0
    })
    .await;
    let detail =
        h.rt.subscribe_session(id, Some(Seq(0)), None)
            .await
            .unwrap()
            .detail;
    let sessions = detail.agent_sessions;
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[1].started_by, StartedBy::Resume);
    assert_eq!(sessions[0].native_id, sessions[1].native_id);
    assert!(sessions[0].ended_at.is_some());
    assert_eq!(h.host.prompts(id), ["remember walnut", "what was it?"]);
}

#[tokio::test]
async fn in_doubt_blocks_writes_until_the_barrier() {
    for logged in [true, false] {
        let mut h = Harness::new(script()).await;
        let id = h.create().await;
        h.prompt(id, "ask first").await;
        let s = h.wait_state(id, SessionState::AwaitingPermission).await;
        // The answer reaches the supervisor, which hasn't logged it when the daemon dies.
        h.host.hold_writes(id, true);
        h.rt.respond(allow(id, s.requests[0].id.clone()))
            .await
            .unwrap();
        h.host
            .wait("the held answer", |host| host.held_writes(id) == 1)
            .await;
        let h = h.restart().await;
        // Recovered and attached, but in doubt: nothing else was written, answers included.
        assert_eq!(h.host.held_writes(id), 1);
        assert_eq!(count(&h, id, |c| matches!(c, Command::Answer { .. })), 0);
        let mut h = h;
        if logged {
            h.host.release_writes(id);
        } else {
            h.host.drop_writes(id);
        }
        h.wait(id, "the run end", |s| s.preview == "deployed fine")
            .await;
        assert_eq!(count(&h, id, |c| matches!(c, Command::Answer { .. })), 1);
    }
}

#[tokio::test]
async fn an_in_doubt_prompt_is_sent_exactly_once() {
    for logged in [true, false] {
        let mut h = Harness::new(script()).await;
        let id = h.create().await;
        h.host.hold_writes(id, true);
        assert_eq!(h.prompt(id, "held").await, PromptResult::Sent);
        h.host
            .wait("the held prompt", |host| host.held_writes(id) == 1)
            .await;
        let mut h = h.restart().await;
        assert_eq!(h.summary(id).state, SessionState::Working, "still sending");
        assert!(h.host.prompts(id).is_empty());
        if logged {
            h.host.release_writes(id);
        } else {
            h.host.drop_writes(id);
        }
        h.wait(id, "the run end", |s| {
            s.preview == "queued reply" && s.state == SessionState::Idle
        })
        .await;
        assert_eq!(h.host.prompts(id), ["held"]);
    }
}

#[tokio::test]
async fn a_failed_commit_discards_outbound_lines() {
    let mut h = Harness::new(script()).await;
    let id = h.create().await;
    h.rt.inner()
        .with_core(|core| core.store.fail_next_commit = true);
    let params = SessionPromptParams {
        op_id: OpId::new("prompt-1"),
        id,
        content: text("doomed"),
        origin: PromptOrigin::default(),
        if_busy: IfBusy::Queue,
    };
    let err = h.rt.prompt(params.clone()).await.unwrap_err();
    assert_eq!(err.data, Some(ErrorData::InternalError));
    assert_eq!(h.summary(id).queue.len, 0, "the queue change rolled back");
    assert!(
        h.host.prompts(id).is_empty(),
        "the staged line was discarded"
    );

    // The op id wasn't recorded either: a retry runs, once.
    h.rt.prompt(params).await.unwrap();
    h.wait(id, "the run end", |s| {
        s.preview == "queued reply" && s.state == SessionState::Idle
    })
    .await;
    assert_eq!(h.host.prompts(id), ["doomed"]);
}

#[tokio::test]
async fn a_failed_write_returns_the_item_and_pauses() {
    let mut h = Harness::new(script()).await;
    let id = h.create().await;
    h.host.hold_writes(id, true);
    h.prompt(id, "lost").await;
    h.host
        .wait("the held prompt", |host| host.held_writes(id) == 1)
        .await;
    h.host.drop_writes(id);
    let s = h
        .wait(id, "the item back", |s| {
            s.queue.paused && s.state == SessionState::Idle
        })
        .await;
    assert_eq!(s.queue.len, 1);
    assert!(!h.queue(id).await[0].sending);
    assert!(h.host.prompts(id).is_empty());
    h.rt.queue_resume(h.op(id)).await.unwrap();
    h.wait(id, "sent", |s| {
        s.queue.len == 0 && s.state == SessionState::Idle
    })
    .await;
    assert_eq!(h.host.prompts(id), ["lost"]);
}
