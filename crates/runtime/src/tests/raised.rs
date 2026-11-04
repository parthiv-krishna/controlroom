//! Sandbox approvals from a workspace's host: shown as a blocking `sandbox` request, answered
//! once (the first decision wins and goes back to the host), denied when the session cancels,
//! ended when the host closes them, kept across an agent daemon restart, and reconciled with
//! the host's pending list after a reconnect.

use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::ErrorData;
use controlroom_protocol::agentd::RequestRespondParams;
use controlroom_protocol::host::{ApprovalClosedParams, HostNotification};
use controlroom_protocol::ids::{ApprovalId, DeviceId, OpId, RequestId, SessionId, SubscriptionId};
use controlroom_protocol::session::{
    Attention, DeviceRef, RequestKind, RequestState, Response, SessionState,
};

use super::Harness;

fn request_id(approval: ApprovalId) -> RequestId {
    RequestId::new(format!("approval-{approval}"))
}

fn choose(id: SessionId, approval: ApprovalId, option: &str) -> RequestRespondParams {
    RequestRespondParams {
        op_id: OpId::generate(),
        id,
        request_id: request_id(approval),
        response: Response::Choice {
            option_id: option.into(),
            reason: None,
        },
        then: None,
        by: Some(DeviceRef {
            device_id: DeviceId::new("phone"),
            label: "Phone".into(),
        }),
    }
}

/// Raises an approval in the session's primary workspace and waits until it is shown.
async fn raise(h: &mut Harness, id: SessionId) -> ApprovalId {
    let workspace = h.summary(id).workspaces[0].id;
    let devbox = h.devbox;
    // The approvals subscription of the current connection is in place first.
    h.machines
        .wait("approvals.subscribe", move |m| {
            m.methods(devbox)
                .contains(&"approvals.subscribe".to_owned())
        })
        .await;
    let approval = h.machines.approval(devbox, id, workspace);
    h.wait_state(id, SessionState::AwaitingPermission).await;
    approval
}

#[tokio::test]
async fn a_host_approval_is_shown_answered_once_and_sent_back() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let approval = raise(&mut h, id).await;
    let s = h.summary(id);
    assert_eq!(s.attention, Some(Attention::Approval));
    assert_eq!(s.requests[0].kind, RequestKind::Sandbox);
    assert!(s.requests[0].blocking);

    let answered =
        h.rt.respond(choose(id, approval, "allow-once"))
            .await
            .unwrap();
    assert_eq!(answered.request.state, RequestState::Answered);
    let err =
        h.rt.respond(choose(id, approval, "deny"))
            .await
            .unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::AlreadyResolved { .. })));
    let devbox = h.devbox;
    h.machines
        .wait("the answer", move |m| m.answers(devbox).len() == 1)
        .await;
    assert_eq!(h.machines.answers(devbox)[0].option_id, "allow-once");
    let s = h.wait_state(id, SessionState::Idle).await;
    assert!(s.requests.is_empty());
}

#[tokio::test]
async fn a_cancel_denies_and_the_host_can_close_an_approval() {
    let script = FakeScript::default().turn(
        "push",
        [FakeStep::Tool {
            title: "git push".into(),
            output: "pushed".into(),
            gate: Some("push".into()),
        }],
    );
    let mut h = Harness::new(script).await;
    let id = h.create().await;
    h.prompt(id, "push").await;
    let approval = raise(&mut h, id).await;
    h.rt.cancel(h.op(id)).await.unwrap();
    let devbox = h.devbox;
    h.machines
        .wait("the denial", move |m| {
            m.answers(devbox).iter().any(|a| a.option_id == "deny")
        })
        .await;
    assert_eq!(h.machines.answers(devbox)[0].approval_id, approval);
    h.host.release(id, "push");
    h.wait_state(id, SessionState::Idle).await;

    // nono gave up on another one: it ends without an answer.
    let other = raise(&mut h, id).await;
    h.rt.hostops_notify(
        devbox,
        HostNotification::ApprovalClosed(ApprovalClosedParams {
            subscription_id: SubscriptionId::generate(),
            approval_id: other,
            state: RequestState::Expired,
        }),
    );
    let s = h.wait_state(id, SessionState::Idle).await;
    assert!(s.requests.is_empty());
    assert_eq!(h.machines.answers(devbox).len(), 1, "nothing sent back");
}

#[tokio::test]
async fn approvals_survive_a_restart_and_follow_the_hosts_list() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let approval = raise(&mut h, id).await;
    let mut h = h.restart().await;
    // Still pending after the restart: the host still waits for it.
    let s = h.wait_state(id, SessionState::AwaitingPermission).await;
    assert_eq!(s.requests[0].id, request_id(approval));
    h.rt.respond(choose(id, approval, "deny")).await.unwrap();
    h.wait_state(id, SessionState::Idle).await;

    // One the host no longer lists after a reconnect is expired.
    let gone = raise(&mut h, id).await;
    let devbox = h.devbox;
    h.machines.drop_link(devbox);
    h.machines.forget_approvals(devbox);
    h.machines.reconnect(devbox);
    let s = h.wait_state(id, SessionState::Idle).await;
    assert!(s.requests.iter().all(|r| r.id != request_id(gone)));
}
