//! Requests: approve and deny, racing decisions, deny-and-steer, questions, roll-ups.

use controlroom_drivers::Capabilities;
use controlroom_drivers::fake::wire::{Command, Output};
use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::ErrorData;
use controlroom_protocol::agentd::{PromptResult, RequestRespondParams, SessionCreateParams};
use controlroom_protocol::ids::{DeviceId, OpId, RequestId, SessionId};
use controlroom_protocol::session::{
    Attention, DeviceRef, RequestKind, RequestState, Response, RunOutcome, SessionState,
};
use controlroom_transcript::Body;

use super::{Harness, caps, text};

fn permission_script() -> FakeScript {
    FakeScript::default().default_turn([
        FakeStep::Request {
            kind: RequestKind::Permission,
            title: "rm -rf build".into(),
        },
        FakeStep::Tool {
            title: "rm -rf build".into(),
            output: "removed".into(),
            gate: None,
        },
        FakeStep::Text("done".into()),
    ])
}

fn respond(id: SessionId, request_id: RequestId, option: &str) -> RequestRespondParams {
    RequestRespondParams {
        op_id: OpId::generate(),
        id,
        request_id,
        response: Response::Choice {
            option_id: option.into(),
            reason: None,
        },
        then: None,
        by: None,
    }
}

fn answers(h: &Harness, id: SessionId) -> usize {
    h.host
        .commands(id)
        .iter()
        .filter(|c| matches!(c, Command::Answer { .. }))
        .count()
}

/// Tool outcomes the agent reported, in order.
fn tool_results(h: &Harness, id: SessionId) -> Vec<(bool, String)> {
    h.host
        .transcript(id)
        .iter()
        .filter_map(|r| match &r.body {
            Body::Out(line) => match serde_json::from_str(&line.raw) {
                Ok(Output::ToolEnd { ok, output, .. }) => Some((ok, output)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn permission_approve_and_deny() {
    let mut h = Harness::new(permission_script()).await;
    let id = h.create().await;

    h.prompt(id, "clean up").await;
    let s = h.wait_state(id, SessionState::AwaitingPermission).await;
    assert_eq!(s.preview, "rm -rf build");
    assert_eq!(s.attention, Some(Attention::Approval));
    // The deciding device is recorded with the answer.
    let laptop = DeviceRef {
        device_id: DeviceId::new("laptop-1"),
        label: "Laptop".into(),
    };
    let result =
        h.rt.respond(RequestRespondParams {
            by: Some(laptop.clone()),
            ..respond(id, s.requests[0].id.clone(), "allow-once")
        })
        .await
        .unwrap();
    assert_eq!(result.request.state, RequestState::Answered);
    let answer = result.request.answer.unwrap();
    assert_eq!(answer.label, "Allow once");
    assert_eq!(answer.by, Some(laptop));
    assert_eq!(result.then, None);
    let s = h
        .wait(id, "first run", |s| {
            s.last_outcome == Some(RunOutcome::Completed)
        })
        .await;
    assert_eq!(s.preview, "done");
    assert_eq!(tool_results(&h, id), [(true, "removed".to_owned())]);

    // Deny: the tool fails.
    let first_end = s.last_turn_end_seq;
    h.prompt(id, "again").await;
    let s = h.wait_state(id, SessionState::AwaitingPermission).await;
    h.rt.respond(respond(id, s.requests[0].id.clone(), "reject"))
        .await
        .unwrap();
    h.wait(id, "second run", |s| {
        s.last_turn_end_seq > first_end && s.state == SessionState::Idle
    })
    .await;
    assert_eq!(
        tool_results(&h, id)[1],
        (false, "Permission denied".to_owned())
    );

    // An option the request doesn't offer is refused.
    h.prompt(id, "third").await;
    let s = h.wait_state(id, SessionState::AwaitingPermission).await;
    let err =
        h.rt.respond(respond(id, s.requests[0].id.clone(), "bogus"))
            .await
            .unwrap_err();
    assert_eq!(err.data, Some(ErrorData::InvalidParams));
    assert_eq!(answers(&h, id), 2);
}

#[tokio::test]
async fn two_clients_race_a_decision() {
    let mut h = Harness::new(permission_script()).await;
    let id = h.create().await;
    h.prompt(id, "go").await;
    let s = h.wait_state(id, SessionState::AwaitingPermission).await;
    let request_id = s.requests[0].id.clone();

    let (phone, laptop) = tokio::join!(
        h.rt.respond(respond(id, request_id.clone(), "allow-once")),
        h.rt.respond(respond(id, request_id.clone(), "reject")),
    );
    let (winner, loser) = match (phone, laptop) {
        (Ok(w), Err(l)) | (Err(l), Ok(w)) => (w, l),
        other => panic!("exactly one decision must win: {other:?}"),
    };
    let Some(ErrorData::AlreadyResolved { request }) = loser.data else {
        panic!("{loser:?}");
    };
    assert_eq!(*request, winner.request);

    h.wait(id, "run end", |s| {
        s.last_outcome.is_some() && s.state == SessionState::Idle
    })
    .await;
    assert_eq!(answers(&h, id), 1, "the agent got exactly one answer");
    // A late answer still learns the winner.
    let err =
        h.rt.respond(respond(id, request_id, "allow-once"))
            .await
            .unwrap_err();
    let Some(ErrorData::AlreadyResolved { request }) = err.data else {
        panic!("{err:?}");
    };
    assert_eq!(*request, winner.request);
}

#[tokio::test]
async fn deny_and_steer_is_one_command() {
    for steer in [false, true] {
        let script = FakeScript::default()
            .turn("instead", [FakeStep::Text("did it".into())])
            .default_turn([
                FakeStep::Request {
                    kind: RequestKind::Permission,
                    title: "rm".into(),
                },
                FakeStep::Tool {
                    title: "rm".into(),
                    output: "removed".into(),
                    gate: None,
                },
                FakeStep::Gate("after".into()),
                FakeStep::Text("done".into()),
            ]);
        let mut h = Harness::with(script, Capabilities { steer, ..caps() }).await;
        let id = h.create().await;
        h.prompt(id, "clean up").await;
        let s = h.wait_state(id, SessionState::AwaitingPermission).await;
        let mut params = respond(id, s.requests[0].id.clone(), "reject");
        params.then = Some(text("do this instead"));
        let first = h.rt.respond(params.clone()).await.unwrap();
        // Repeating the command (a reconnect) applies neither half again.
        assert_eq!(h.rt.respond(params).await.unwrap(), first);
        if steer {
            assert_eq!(first.then, Some(PromptResult::Steered));
            h.wait(id, "steered", |s| {
                s.preview.starts_with("Steered: do this instead")
            })
            .await;
            h.host.release(id, "after");
            h.wait(id, "run end", |s| s.state == SessionState::Idle)
                .await;
            assert_eq!(h.host.prompts(id), ["clean up"]);
        } else {
            assert!(matches!(first.then, Some(PromptResult::Queued { .. })));
            // No open tool after the denial: the run is cancelled at once for the prompt.
            h.wait(id, "the steer prompt", |s| {
                s.preview == "did it" && s.state == SessionState::Idle
            })
            .await;
            assert_eq!(h.host.prompts(id), ["clean up", "do this instead"]);
        }
        assert_eq!(answers(&h, id), 1);
    }
}

#[tokio::test]
async fn questions_are_non_blocking_and_can_be_dismissed() {
    let mut h = Harness::new(FakeScript::default().default_turn([
        FakeStep::Request {
            kind: RequestKind::Question,
            title: "Which **color**?".into(),
        },
        FakeStep::Text("carrying on".into()),
    ]))
    .await;
    let id = h.create().await;
    h.prompt(id, "ask me").await;
    let s = h.wait(id, "the question", |s| !s.requests.is_empty()).await;
    assert_eq!(s.state, SessionState::Working, "questions don't block");
    assert_eq!(s.attention, Some(Attention::Question));
    assert_eq!(s.preview, "Which color?");
    assert!(!s.requests[0].blocking);
    // Only questions can be dismissed.
    let mut dismiss = respond(id, s.requests[0].id.clone(), "");
    dismiss.response = Response::Dismiss;
    let result = h.rt.respond(dismiss).await.unwrap();
    assert_eq!(result.request.answer.unwrap().label, "Dismissed");
    h.wait(id, "run end", |s| {
        s.last_outcome == Some(RunOutcome::Completed)
    })
    .await;
}

#[tokio::test]
async fn child_attention_rolls_up_to_the_parent() {
    let mut h = Harness::new(permission_script()).await;
    let parent = h.create().await;
    let child =
        h.rt.create(SessionCreateParams {
            name: Some("child".into()),
            parent_id: Some(parent),
            ..h.create_params(&[(h.devbox, "~/src/app/sub")])
        })
        .await
        .unwrap()
        .session
        .id;
    h.wait_state(child, SessionState::Idle).await;
    h.prompt(child, "go").await;
    h.wait_state(child, SessionState::AwaitingPermission).await;
    let s = h
        .wait(parent, "the roll-up", |s| s.rollup.approvals == 1)
        .await;
    assert_eq!(s.requests.len(), 1);
    assert_eq!(s.requests[0].origin_child, Some(child));
    assert_eq!(s.attention, None, "the parent itself needs nothing");
}
