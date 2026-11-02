//! Requests the daemon raises itself (sandbox approvals): shown like any request, answered
//! once (first decision wins, with the deciding device), expired when given up or after a
//! restart, withdrawn by a cancel, and never sent to the agent.

use controlroom_drivers::fake::wire::Command;
use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::ErrorData;
use controlroom_protocol::daemon::{QueueSendNowParams, RequestRespondParams, SendNowResult};
use controlroom_protocol::ids::{DeviceId, OpId, RequestId, Seq, SessionId};
use controlroom_protocol::session::{
    Attention, DeviceRef, OptionStyle, Request, RequestBody, RequestKind, RequestOption,
    RequestState, Response, SessionState,
};
use controlroom_protocol::timeline::ItemBody;

use super::Harness;
use crate::RaiseRequest;

fn git_push() -> RaiseRequest {
    RaiseRequest {
        kind: RequestKind::Sandbox,
        title: "Run `git push origin main`?".into(),
        detail: None,
        body: Some(RequestBody::GitPush {
            command: "git push origin main".into(),
            remote: "origin".into(),
            branch: "main".into(),
            force: false,
        }),
        options: vec![
            RequestOption {
                id: "allow-once".into(),
                label: "Allow once".into(),
                style: OptionStyle::Allow,
            },
            RequestOption {
                id: "deny".into(),
                label: "Deny".into(),
                style: OptionStyle::Deny,
            },
        ],
    }
}

fn choose(id: SessionId, request: &RequestId, option: &str) -> RequestRespondParams {
    RequestRespondParams {
        op_id: OpId::generate(),
        id,
        request_id: request.clone(),
        response: Response::Choice {
            option_id: option.into(),
            reason: None,
        },
        then: None,
        by: None,
    }
}

fn already_resolved(err: controlroom_protocol::RpcError) -> Request {
    match err.data {
        Some(ErrorData::AlreadyResolved { request }) => *request,
        other => panic!("expected already_resolved, got {other:?}"),
    }
}

/// The request as its timeline item says, from a fresh subscription at `since`.
async fn item(h: &Harness, id: SessionId, request: &RequestId, since: Seq) -> Option<Request> {
    let sub = h.rt.subscribe_session(id, Some(since), None).await.unwrap();
    sub.backlog.into_iter().find_map(|item| match item.body {
        ItemBody::Request { request: r } if r.id == *request => Some(r),
        _ => None,
    })
}

fn tool_script() -> FakeScript {
    FakeScript::default()
        .turn(
            "push",
            [
                FakeStep::Tool {
                    title: "git push origin main".into(),
                    output: "pushed".into(),
                    gate: Some("push".into()),
                },
                FakeStep::Text("pushed it".into()),
            ],
        )
        .default_turn([FakeStep::Text("next one handled".into())])
}

#[tokio::test]
async fn a_sandbox_approval_is_shown_answered_once_and_attributed() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let head =
        h.rt.subscribe_session(id, None, None)
            .await
            .unwrap()
            .head_seq;

    let mut raised = h.rt.raise(id, git_push()).await.unwrap();
    let request_id = raised.request().id.clone();
    let s = h.wait_state(id, SessionState::AwaitingPermission).await;
    assert_eq!(s.attention, Some(Attention::Approval));
    assert_eq!(
        s.preview, "Run git push origin main?",
        "the preview is plain text"
    );
    assert_eq!(s.requests.len(), 1);
    assert_eq!(s.requests[0].kind, RequestKind::Sandbox);
    assert!(s.requests[0].blocking);
    // A client resuming from the head it had before still gets the item.
    let shown = item(&h, id, &request_id, head)
        .await
        .expect("a timeline item");
    assert_eq!(shown.state, RequestState::Pending);

    // An option it doesn't offer is refused; a valid answer resolves it with the device.
    let err =
        h.rt.respond(choose(id, &request_id, "allow-forever"))
            .await
            .unwrap_err();
    assert_eq!(err.data, Some(ErrorData::InvalidParams));
    let phone = DeviceRef {
        device_id: DeviceId::new("phone-1"),
        label: "Phone".into(),
    };
    let answered =
        h.rt.respond(RequestRespondParams {
            by: Some(phone.clone()),
            ..choose(id, &request_id, "allow-once")
        })
        .await
        .unwrap();
    assert_eq!(answered.request.state, RequestState::Answered);
    let resolution = raised.resolution().await;
    assert_eq!(resolution, answered.request);
    let answer = resolution.answer.unwrap();
    assert_eq!(answer.by, Some(phone));
    assert_eq!(answer.label, "Allow once");
    let s = h.wait_state(id, SessionState::Idle).await;
    assert!(s.requests.is_empty());
    assert_eq!(
        item(&h, id, &request_id, head).await.unwrap().state,
        RequestState::Answered
    );
    // It never reached the agent; giving up afterwards changes nothing.
    assert!(
        !h.host
            .commands(id)
            .iter()
            .any(|c| matches!(c, Command::Answer { .. }))
    );
    assert_eq!(raised.expire().state, RequestState::Answered);
}

#[tokio::test]
async fn two_clients_race_a_sandbox_approval() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let mut raised = h.rt.raise(id, git_push()).await.unwrap();
    let request_id = raised.request().id.clone();
    let (a, b) = tokio::join!(
        h.rt.respond(choose(id, &request_id, "allow-once")),
        h.rt.respond(choose(id, &request_id, "deny")),
    );
    let (winner, loser) = match (a, b) {
        (Ok(w), Err(l)) | (Err(l), Ok(w)) => (w, l),
        other => panic!("exactly one decision must win: {other:?}"),
    };
    assert_eq!(already_resolved(loser), winner.request);
    assert_eq!(raised.resolution().await, winner.request);
}

#[tokio::test]
async fn an_answer_racing_the_timeout_has_one_outcome() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    for _ in 0..20 {
        let raised = h.rt.raise(id, git_push()).await.unwrap();
        let request_id = raised.request().id.clone();
        let rt = h.rt.clone();
        let answer = tokio::spawn(async move { rt.respond(choose(id, &request_id, "deny")).await });
        let expired = tokio::task::spawn_blocking(move || raised.expire())
            .await
            .unwrap();
        match answer.await.unwrap() {
            Ok(won) => assert_eq!(expired, won.request),
            Err(lost) => {
                assert_eq!(expired.state, RequestState::Expired);
                assert_eq!(already_resolved(lost), expired);
            }
        }
    }
    let s = h.wait_state(id, SessionState::Idle).await;
    assert!(s.requests.is_empty());
}

#[tokio::test]
async fn a_given_up_approval_expires_and_denies_late_answers() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let raised = h.rt.raise(id, git_push()).await.unwrap();
    let request_id = raised.request().id.clone();
    h.wait_state(id, SessionState::AwaitingPermission).await;
    let expired = raised.expire();
    assert_eq!(expired.state, RequestState::Expired);
    assert_eq!(expired.answer, None);
    h.wait_state(id, SessionState::Idle).await;
    let late = h.rt.respond(choose(id, &request_id, "allow-once")).await;
    assert_eq!(already_resolved(late.unwrap_err()), expired);

    // Dropping the handle (e.g. the HTTP request behind it died) expires it too.
    let dropped = h.rt.raise(id, git_push()).await.unwrap();
    let dropped_id = dropped.request().id.clone();
    drop(dropped);
    let late = h.rt.respond(choose(id, &dropped_id, "allow-once")).await;
    assert_eq!(
        already_resolved(late.unwrap_err()).state,
        RequestState::Expired
    );
}

#[tokio::test]
async fn a_restart_expires_pending_approvals() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let raised = h.rt.raise(id, git_push()).await.unwrap();
    let request_id = raised.request().id.clone();
    h.wait_state(id, SessionState::AwaitingPermission).await;
    // The daemon dies with the HTTP request: nobody gets to expire it before the restart.
    std::mem::forget(raised);
    let mut h = h.restart().await;
    let s = h.wait_state(id, SessionState::Idle).await;
    assert!(s.requests.is_empty());
    assert_eq!(s.attention, None);
    assert_eq!(
        item(&h, id, &request_id, Seq(0)).await.unwrap().state,
        RequestState::Expired
    );
    let late = h.rt.respond(choose(id, &request_id, "allow-once")).await;
    assert_eq!(
        already_resolved(late.unwrap_err()).state,
        RequestState::Expired
    );
}

#[tokio::test]
async fn cancel_and_send_now_withdraw_an_approval_during_its_tool() {
    let mut h = Harness::new(tool_script()).await;
    let id = h.create().await;

    // A send-now doesn't wait for a tool that waits for an approval: the approval is withdrawn
    // and the run cancelled.
    h.prompt(id, "push it").await;
    h.wait(id, "the tool", |s| s.preview == "git push origin main")
        .await;
    let mut raised = h.rt.raise(id, git_push()).await.unwrap();
    h.wait_state(id, SessionState::AwaitingPermission).await;
    let next = match h.prompt(id, "do this instead").await {
        controlroom_protocol::daemon::PromptResult::Queued { queue_id } => queue_id,
        other => panic!("{other:?}"),
    };
    let result =
        h.rt.send_now(QueueSendNowParams {
            op_id: OpId::generate(),
            id,
            queue_id: next,
            interrupt: false,
        })
        .await
        .unwrap();
    assert_eq!(result, SendNowResult::Interrupting);
    assert_eq!(raised.resolution().await.state, RequestState::Withdrawn);
    h.wait(id, "the next run", |s| {
        s.preview == "next one handled" && s.state == SessionState::Idle
    })
    .await;
    assert_eq!(h.host.prompts(id), ["push it", "do this instead"]);

    // A cancel withdraws it too.
    h.prompt(id, "push again").await;
    h.wait(id, "the tool", |s| s.preview == "git push origin main")
        .await;
    let mut raised = h.rt.raise(id, git_push()).await.unwrap();
    h.rt.cancel(h.op(id)).await.unwrap();
    assert_eq!(raised.resolution().await.state, RequestState::Withdrawn);
    h.wait(id, "cancelled", |s| {
        s.state == SessionState::Idle && s.requests.is_empty()
    })
    .await;
}

#[tokio::test]
async fn closing_or_deleting_a_session_ends_its_approvals() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let mut raised = h.rt.raise(id, git_push()).await.unwrap();
    h.rt.close(h.op(id)).await.unwrap();
    assert_eq!(raised.resolution().await.state, RequestState::Withdrawn);
    let err = h.rt.raise(id, git_push()).await.err().unwrap();
    assert!(matches!(err.data, Some(ErrorData::InvalidState { .. })));

    let other = h.create().await;
    let mut raised = h.rt.raise(other, git_push()).await.unwrap();
    h.rt.delete(h.op(other)).await.unwrap();
    assert_eq!(raised.resolution().await.state, RequestState::Withdrawn);
    let err = h.rt.raise(other, git_push()).await.err().unwrap();
    assert!(matches!(err.data, Some(ErrorData::UnknownSession { .. })));
}
