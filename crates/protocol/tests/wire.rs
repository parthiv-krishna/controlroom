//! Wire-shape tests: golden JSON for representative messages, serde round-trips, forward
//! compatibility (unknown variants and fields, a bad result for one id), and the method tables.

use std::collections::BTreeSet;

use controlroom_protocol::agentd::methods as dm;
use controlroom_protocol::agentd::*;
use controlroom_protocol::ids::*;
use controlroom_protocol::session::*;
use controlroom_protocol::timeline::*;
use controlroom_protocol::ui::methods as um;
use controlroom_protocol::ui::*;
use controlroom_protocol::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Asserts `value` serializes to exactly `golden` and parses back to itself.
fn golden<T>(value: &T, golden: Value)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_value(value).unwrap();
    assert_eq!(json, golden, "wire shape");
    let back: T = serde_json::from_value(json).unwrap();
    assert_eq!(&back, value, "round-trip");
}

const SID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3E6";
const MID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3E7";
const SUB: &str = "01HZY3K6W8Q9X2V4N7B5C1D3E8";
const DID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3E9";
const CID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3EA";
const WID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3EF";

fn sid() -> SessionId {
    SID.parse().unwrap()
}

fn sub() -> SubscriptionId {
    SUB.parse().unwrap()
}

fn request() -> Request {
    Request {
        id: RequestId::new("r1"),
        kind: RequestKind::Sandbox,
        blocking: true,
        state: RequestState::Pending,
        title: "Run git push origin main?".into(),
        detail: None,
        body: Some(RequestBody::GitPush {
            command: "git push origin main".into(),
            remote: "origin".into(),
            branch: "main".into(),
            force: false,
        }),
        options: vec![
            RequestOption {
                id: "allow".into(),
                label: "Allow once".into(),
                style: OptionStyle::Allow,
            },
            RequestOption {
                id: "deny".into(),
                label: "Deny".into(),
                style: OptionStyle::Deny,
            },
        ],
        questions: vec![],
        answer: None,
        origin_child: None,
        created_at: UnixMillis(2_000),
    }
}

fn summary() -> SessionSummary {
    SessionSummary {
        id: sid(),
        name: Some("fix flaky test".into()),
        title: None,
        workspaces: vec![Workspace {
            id: WID.parse().unwrap(),
            machine_id: MID.parse().unwrap(),
            path: "~/src/app".into(),
            root: Some("/home/alice/src/app".into()),
            primary: true,
            added_at: UnixMillis(1_000),
            availability: Availability {
                connectivity: Connectivity::Connected,
                since: UnixMillis(900),
            },
        }],
        shells: vec![Shell {
            name: "devbox".into(),
            workspace_id: WID.parse().unwrap(),
            cwd: "/home/alice/src/app".into(),
            state: ShellState::Idle,
            reset_pending: false,
            foreground: None,
        }],
        created_at: UnixMillis(1_000),
        updated_at: UnixMillis(2_000),
        closed_at: None,
        parent_id: None,
        state: SessionState::AwaitingPermission,
        last_outcome: Some(RunOutcome::Completed),
        last_error: None,
        read_seq: Seq(10),
        last_turn_end_seq: Seq(12),
        unread: true,
        preview: "Run git push origin main?".into(),
        attention: Some(Attention::Approval),
        rollup: AttentionRollup::default(),
        context: Some(ContextUsage {
            used: 41_000,
            size: Some(200_000),
            cost: Some(0.25),
        }),
        usage: SessionUsage {
            tokens: TokenUsage {
                input: 430_000,
                output: 378_000,
                cache_read: 303_000_000,
                cache_write: 54_000_000,
            },
            cost: 337.7,
            latest_cache_hit: Some(0.993),
        },
        queue: QueueStatus {
            len: 1,
            paused: false,
            auto_send: true,
            send_now_pending: false,
        },
        requests: vec![request()],
        todo: Some(TodoRollup {
            done: 2,
            total: 5,
            auto_continue: AutoContinueStatus::On,
        }),
        live_work: LiveWork {
            jobs: 1,
            children: 0,
            asks: 0,
            host_calls: 0,
        },
        options: vec![SessionOption {
            id: "thinking".into(),
            label: "Thinking".into(),
            control: OptionControl::Select {
                value: "medium".into(),
                choices: vec![OptionChoice {
                    value: "medium".into(),
                    label: "Medium".into(),
                }],
            },
        }],
        agent_live: true,
        sandbox: SandboxInfo {
            level: SandboxLevel::Relaxed,
            default_level: SandboxLevel::Strict,
            explicit: true,
            looser_than_default: true,
            grants: vec![],
        },
        resumable: true,
    }
}

fn summary_json() -> Value {
    json!({
        "id": SID,
        "name": "fix flaky test",
        "title": null,
        "workspaces": [{
            "id": WID,
            "machine_id": MID,
            "path": "~/src/app",
            "root": "/home/alice/src/app",
            "primary": true,
            "added_at": 1000,
            "availability": {"connectivity": {"state": "connected"}, "since": 900}
        }],
        "shells": [{
            "name": "devbox",
            "workspace_id": WID,
            "cwd": "/home/alice/src/app",
            "state": {"state": "idle"},
            "reset_pending": false,
            "foreground": null
        }],
        "created_at": 1000,
        "updated_at": 2000,
        "closed_at": null,
        "parent_id": null,
        "state": "awaiting_permission",
        "last_outcome": "completed",
        "last_error": null,
        "read_seq": 10,
        "last_turn_end_seq": 12,
        "unread": true,
        "preview": "Run git push origin main?",
        "attention": "approval",
        "rollup": {"approvals": 0, "questions": 0, "problems": 0, "unread": 0},
        "context": {"used": 41000, "size": 200000, "cost": 0.25},
        "usage": {
            "tokens": {"input": 430000, "output": 378000, "cache_read": 303000000, "cache_write": 54000000},
            "cost": 337.7,
            "latest_cache_hit": 0.993
        },
        "queue": {"len": 1, "paused": false, "auto_send": true, "send_now_pending": false},
        "requests": [{
            "id": "r1",
            "kind": "sandbox",
            "blocking": true,
            "state": "pending",
            "title": "Run git push origin main?",
            "detail": null,
            "body": {"kind": "git_push", "command": "git push origin main", "remote": "origin", "branch": "main", "force": false},
            "options": [
                {"id": "allow", "label": "Allow once", "style": "allow"},
                {"id": "deny", "label": "Deny", "style": "deny"}
            ],
            "questions": [],
            "answer": null,
            "origin_child": null,
            "created_at": 2000
        }],
        "todo": {"done": 2, "total": 5, "auto_continue": "on"},
        "live_work": {"jobs": 1, "children": 0, "asks": 0, "host_calls": 0},
        "options": [{
            "id": "thinking",
            "label": "Thinking",
            "kind": "select",
            "value": "medium",
            "choices": [{"value": "medium", "label": "Medium"}]
        }],
        "agent_live": true,
        "sandbox": {
            "level": "relaxed",
            "default_level": "strict",
            "explicit": true,
            "looser_than_default": true,
            "grants": []
        },
        "resumable": true
    })
}

// ---------------------------------------------------------------------------------------------
// Golden messages
// ---------------------------------------------------------------------------------------------

#[test]
fn golden_agentd_hello_request() {
    let request = RpcRequest::new::<dm::Hello>(
        1,
        HelloParams {
            protocol_version: AGENTD_PROTOCOL_VERSION,
            client_name: "controlroom-web 0.1.0".into(),
            role: ClientRole::Controller {
                controller_id: CID.parse().unwrap(),
            },
        },
    );
    golden(
        &request,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "hello",
            "params": {
                "protocol_version": {"major": 1, "minor": 0},
                "client_name": "controlroom-web 0.1.0",
                "role": {"kind": "controller", "controller_id": CID}
            }
        }),
    );
    let debug: HelloParams = serde_json::from_value(json!({
        "protocol_version": {"major": 1, "minor": 0},
        "client_name": "controlroom-agentd ctl",
        "role": {"kind": "debug"}
    }))
    .unwrap();
    assert_eq!(debug.role, ClientRole::Debug);
}

#[test]
fn golden_controller_conflict_error() {
    let line = format!(
        r#"{{"jsonrpc":"2.0","id":1,"error":{{"code":-32019,"message":"another controller is connected","data":{{"kind":"controller_conflict","controller_id":"{CID}","connected_since":1700000000000}}}}}}"#
    );
    let Ok(Message::Response(response)) = Message::parse(&line) else {
        panic!("expected a response");
    };
    let err = response.into_typed::<dm::Hello>().unwrap_err();
    let expected = ErrorData::ControllerConflict {
        controller_id: CID.parse().unwrap(),
        connected_since: UnixMillis(1_700_000_000_000),
    };
    assert_eq!(err.code, expected.code());
    assert_eq!(err.data, Some(expected));

    // A conflict without its fields (or from a different build) still fails only this call.
    let malformed = r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32019,"message":"x","data":{"kind":"controller_conflict"}}}"#;
    let Ok(Message::Response(response)) = Message::parse(malformed) else {
        panic!("expected a response");
    };
    assert_eq!(response.id, Some(RpcId::Number(2)));
    assert_eq!(
        response.into_result().unwrap_err().data,
        Some(ErrorData::Unknown)
    );
}

#[test]
fn sandbox_override_fields() {
    let create: SessionCreateParams = serde_json::from_value(json!({
        "op_id": "op-9",
        "workspaces": [{"machine_id": MID, "path": "~/src/app"}],
        "sandbox_level": "off",
        "sandbox": {"level": "strict", "grants": [], "domains": ["example.com"], "mediated": []}
    }))
    .unwrap();
    assert_eq!(create.sandbox_level, Some(SandboxLevel::Off));
    assert_eq!(
        create.sandbox.as_ref().map(|s| s.level),
        Some(SandboxLevel::Strict)
    );
    let plain: SessionCreateParams = serde_json::from_value(json!({
        "op_id": "op-10",
        "workspaces": [{"machine_id": MID, "path": "~"}]
    }))
    .unwrap();
    assert_eq!((plain.sandbox_level, plain.sandbox), (None, None));

    use SandboxLevel::*;
    assert!(Off.is_looser_than(Strict) && Off.is_looser_than(Relaxed));
    assert!(Relaxed.is_looser_than(Strict));
    assert!(!Strict.is_looser_than(Relaxed) && !Strict.is_looser_than(Strict));
    // An unknown level counts as the strictest.
    assert!(!Strict.is_looser_than(Unknown) && Relaxed.is_looser_than(Unknown));
    assert!(!Unknown.is_looser_than(Strict));
}

#[test]
fn golden_session_events_with_timeline_ops() {
    let params = SessionEventsParams {
        subscription_id: sub(),
        id: sid(),
        head_seq: Seq(8),
        ops: vec![
            TimelineOp::Upsert {
                item: Box::new(TimelineItem {
                    id: ItemId::new("run-5"),
                    seq_from: Seq(5),
                    seq_to: Seq(5),
                    actor: Actor::Root,
                    ts: UnixMillis(3_000),
                    streaming: false,
                    body: ItemBody::Run {
                        trigger: RunTrigger::Schedule {
                            schedule_id: "01HZY3K6W8Q9X2V4N7B5C1D3EB".parse().unwrap(),
                            name: "Inbox triage".into(),
                        },
                        state: RunState::Running,
                        outcome: None,
                        error: None,
                    },
                }),
            },
            TimelineOp::AppendText {
                id: ItemId::new("msg-6"),
                text: "Looking at ".into(),
                seq_from: Seq(7),
                seq_to: Seq(8),
            },
            TimelineOp::End {
                id: ItemId::new("msg-6"),
                seq_from: Seq(8),
                seq_to: Seq(8),
            },
        ],
        raw: vec![],
        detail: None,
    };
    golden(
        &params,
        json!({
            "subscription_id": SUB,
            "id": SID,
            "head_seq": 8,
            "ops": [
                {"op": "upsert", "item": {
                    "id": "run-5", "seq_from": 5, "seq_to": 5, "actor": "root", "ts": 3000,
                    "streaming": false, "kind": "run",
                    "trigger": {"kind": "schedule", "schedule_id": "01HZY3K6W8Q9X2V4N7B5C1D3EB", "name": "Inbox triage"},
                    "state": "running", "outcome": null, "error": null
                }},
                {"op": "append_text", "id": "msg-6", "text": "Looking at ", "seq_from": 7, "seq_to": 8},
                {"op": "end", "id": "msg-6", "seq_from": 8, "seq_to": 8}
            ]
        }),
    );
}

#[test]
fn golden_ui_request_respond_pass_through() {
    let request = RpcRequest::new::<um::RequestRespond>(
        "c7",
        RequestRespondParams {
            op_id: OpId::new("op-1"),
            id: sid(),
            request_id: RequestId::new("r1"),
            response: Response::Choice {
                option_id: "deny".into(),
                reason: Some("push to a branch".into()),
            },
            then: Some(vec![ContentPart::text("push to fix/flaky instead")]),
            by: None,
        },
    );
    golden(
        &request,
        json!({
            "jsonrpc": "2.0",
            "id": "c7",
            "method": "request.respond",
            "params": {
                "op_id": "op-1",
                "id": SID,
                "request_id": "r1",
                "response": {"kind": "choice", "option_id": "deny", "reason": "push to a branch"},
                "then": [{"kind": "text", "text": "push to fix/flaky instead"}]
            }
        }),
    );
}

#[test]
fn golden_agentd_request_respond_carries_the_deciding_device() {
    let params = RequestRespondParams {
        op_id: OpId::new("op-2"),
        id: sid(),
        request_id: RequestId::new("sandbox-1"),
        response: Response::Choice {
            option_id: "allow-once".into(),
            reason: None,
        },
        then: None,
        by: Some(DeviceRef {
            device_id: DeviceId::new("dev-1"),
            label: "Laptop".into(),
        }),
    };
    golden(
        &params,
        json!({
            "op_id": "op-2",
            "id": SID,
            "request_id": "sandbox-1",
            "response": {"kind": "choice", "option_id": "allow-once"},
            "by": {"device_id": "dev-1", "label": "Laptop"}
        }),
    );
}

#[test]
fn golden_session_create_with_a_first_prompt() {
    let params = SessionCreateParams {
        op_id: OpId::new("op-3"),
        workspaces: vec![WorkspaceSpec {
            machine_id: MID.parse().unwrap(),
            path: "~/src/app".into(),
        }],
        name: None,
        parent_id: None,
        sandbox_level: None,
        sandbox: None,
        prompt: Some(InitialPrompt {
            content: vec![ContentPart::text("fix the flaky test")],
            origin: PromptOrigin::User { device: None },
        }),
    };
    golden(
        &params,
        json!({
            "op_id": "op-3",
            "workspaces": [{"machine_id": MID, "path": "~/src/app"}],
            "prompt": {
                "content": [{"kind": "text", "text": "fix the flaky test"}],
                "origin": {"kind": "user", "device": null}
            }
        }),
    );
    let queue_id: QueueId = "01HZY3K6W8Q9X2V4N7B5C1D3ED".parse().unwrap();
    let result = SessionCreateResult {
        session: summary(),
        prompt: Some(PromptResult::Queued { queue_id }),
    };
    golden(
        &result,
        json!({
            "session": summary_json(),
            "prompt": {"outcome": "queued", "queue_id": "01HZY3K6W8Q9X2V4N7B5C1D3ED"}
        }),
    );
}

#[test]
fn golden_send_now_interrupt_and_queued() {
    let params = QueueSendNowParams {
        op_id: OpId::new("op-4"),
        id: sid(),
        queue_id: "01HZY3K6W8Q9X2V4N7B5C1D3ED".parse().unwrap(),
        interrupt: true,
    };
    golden(
        &params,
        json!({
            "op_id": "op-4",
            "id": SID,
            "queue_id": "01HZY3K6W8Q9X2V4N7B5C1D3ED",
            "interrupt": true
        }),
    );
    golden(&SendNowResult::Queued, json!({"outcome": "queued"}));
}

#[test]
fn golden_detail_only_session_subscribe() {
    golden(
        &SessionSubscribeParams {
            id: sid(),
            since_seq: Seq(0),
            raw_since: None,
            timeline: false,
        },
        json!({"id": SID, "since_seq": 0, "timeline": false}),
    );
}

#[test]
fn golden_unknown_option_is_not_found() {
    let err = RpcError::new(
        ErrorData::NotFound {
            resource: Resource::Option,
            id: "model".into(),
        },
        "no option model",
    );
    assert_eq!(
        serde_json::to_value(&err).unwrap(),
        json!({"code": -32007, "message": "no option model",
               "data": {"kind": "not_found", "resource": "option", "id": "model"}})
    );
}

#[test]
fn golden_error_response() {
    let response: RpcResponse<Value> = RpcResponse::error(
        Some(RpcId::Number(9)),
        RpcError::new(
            ErrorData::InvalidState {
                state: SessionState::Closed,
            },
            "session is closed",
        ),
    );
    golden(
        &response,
        json!({
            "jsonrpc": "2.0",
            "id": 9,
            "error": {
                "code": -32014,
                "message": "session is closed",
                "data": {"kind": "invalid_state", "state": "closed"}
            }
        }),
    );
}

#[test]
fn golden_state_event_and_connectivity() {
    let change = StateChange {
        rev: Rev(3),
        event: StateEvent::SessionRemoved { session_id: sid() },
    };
    golden(
        &change,
        json!({"rev": 3, "kind": "session_removed", "session_id": SID}),
    );
    golden(
        &Connectivity::ActionRequired {
            reason: ActionRequiredReason::HostKeyChanged,
            presented_host_key: Some(HostKeyInfo {
                algorithm: "ssh-ed25519".into(),
                fingerprint: "SHA256:abc".into(),
            }),
        },
        json!({
            "state": "action_required",
            "reason": "host_key_changed",
            "presented_host_key": {"algorithm": "ssh-ed25519", "fingerprint": "SHA256:abc"}
        }),
    );
}

#[test]
fn actors_are_root_or_a_child_id() {
    assert_eq!(serde_json::to_value(Actor::Root).unwrap(), json!("root"));
    assert_eq!(
        serde_json::to_value(Actor::Child(sid())).unwrap(),
        json!(SID)
    );
    assert_eq!(
        serde_json::from_value::<Actor>(json!(SID)).unwrap(),
        Actor::Child(sid())
    );
    assert!(serde_json::from_value::<Actor>(json!("nobody")).is_err());
}

#[test]
fn optional_params_may_be_omitted() {
    let params: SessionPromptParams = serde_json::from_value(json!({
        "op_id": "op-2", "id": SID, "content": [{"kind": "text", "text": "hi"}]
    }))
    .unwrap();
    assert_eq!(params.origin, PromptOrigin::User { device: None });
    assert_eq!(params.if_busy, IfBusy::Queue);
    let follow: JobFollowParams = serde_json::from_value(json!({
        "op_id": "op-3", "id": SID, "job_id": DID
    }))
    .unwrap();
    assert_eq!(follow.from, FollowFrom::Unseen);
    assert_eq!(follow.buffer_seconds, None);

    // P4 additions are optional params: older clients keep working.
    let create: SessionCreateParams = serde_json::from_value(json!({
        "op_id": "op-4",
        "workspaces": [{"machine_id": MID, "path": "~"}]
    }))
    .unwrap();
    assert_eq!(create.prompt, None);
    let first: InitialPrompt =
        serde_json::from_value(json!({"content": [{"kind": "text", "text": "hi"}]})).unwrap();
    assert_eq!(first.origin, PromptOrigin::User { device: None });
    let subscribe: SessionSubscribeParams =
        serde_json::from_value(json!({"id": SID, "since_seq": 4})).unwrap();
    assert!(subscribe.timeline, "the timeline is on by default");
    let send_now: QueueSendNowParams = serde_json::from_value(json!({
        "op_id": "op-5", "id": SID, "queue_id": "01HZY3K6W8Q9X2V4N7B5C1D3ED"
    }))
    .unwrap();
    assert!(!send_now.interrupt);
    let respond: RequestRespondParams = serde_json::from_value(json!({
        "op_id": "op-6", "id": SID, "request_id": "r1", "response": {"kind": "dismiss"}
    }))
    .unwrap();
    assert_eq!(respond.by, None);
}

// ---------------------------------------------------------------------------------------------
// Forward compatibility
// ---------------------------------------------------------------------------------------------

#[test]
fn unknown_enum_values_decode_as_unknown() {
    let mut newer = summary_json();
    newer["state"] = json!("hibernating");
    newer["attention"] = json!("celebrate");
    newer["last_outcome"] = json!("context_overflow");
    newer["requests"][0]["kind"] = json!("budget_raise");
    newer["requests"][0]["body"] = json!({"kind": "terminal", "term_id": "t1"});
    newer["requests"][0]["options"][0]["style"] = json!("sparkle");
    newer["options"][0] = json!({"id": "x", "label": "X", "kind": "slider", "min": 0});
    newer["sandbox"]["level"] = json!("paranoid");
    newer["some_new_field"] = json!({"nested": true});

    let decoded: SessionSummary = serde_json::from_value(newer).unwrap();
    assert_eq!(decoded.state, SessionState::Unknown);
    assert_eq!(decoded.attention, Some(Attention::Unknown));
    assert_eq!(decoded.last_outcome, Some(RunOutcome::Unknown));
    assert_eq!(decoded.requests[0].kind, RequestKind::Unknown);
    assert_eq!(decoded.requests[0].body, Some(RequestBody::Unknown));
    assert_eq!(decoded.requests[0].options[0].style, OptionStyle::Unknown);
    assert_eq!(decoded.options[0].control, OptionControl::Unknown);
    assert_eq!(decoded.sandbox.level, SandboxLevel::Unknown);
}

#[test]
fn a_summary_without_usage_decodes_with_zero_usage() {
    let mut older = summary_json();
    older.as_object_mut().unwrap().remove("usage");
    let decoded: SessionSummary = serde_json::from_value(older).unwrap();
    assert_eq!(decoded.usage, SessionUsage::default());
}

#[test]
fn unknown_timeline_kinds_and_ops_decode_as_unknown() {
    let params: SessionEventsParams = serde_json::from_value(json!({
        "subscription_id": SUB,
        "id": SID,
        "head_seq": 20,
        "ops": [
            {"op": "upsert", "item": {
                "id": "t-1", "seq_from": 19, "seq_to": 19, "actor": "root", "ts": 1,
                "streaming": false, "kind": "terminal_session", "term_id": "x"
            }},
            {"op": "upsert", "item": {
                "id": "t-2", "seq_from": 19, "seq_to": 20, "actor": "root", "ts": 1,
                "streaming": false, "kind": "run",
                "trigger": {"kind": "webhook", "url": "https://example.com"},
                "state": "paused", "outcome": null, "error": null
            }},
            {"op": "truncate", "id": "t-1", "before": 3}
        ],
        "future": [1, 2, 3]
    }))
    .unwrap();
    let TimelineOp::Upsert { item } = &params.ops[0] else {
        panic!("expected an upsert");
    };
    assert_eq!(item.body, ItemBody::Unknown);
    assert_eq!(item.id, ItemId::new("t-1"));
    let TimelineOp::Upsert { item } = &params.ops[1] else {
        panic!("expected an upsert");
    };
    assert_eq!(
        item.body,
        ItemBody::Run {
            trigger: RunTrigger::Unknown,
            state: RunState::Unknown,
            outcome: None,
            error: None,
        }
    );
    assert_eq!(params.ops[2], TimelineOp::Unknown);
}

#[test]
fn unknown_feed_events_decode_as_unknown() {
    let change: SessionsChange =
        serde_json::from_value(json!({"rev": 7, "kind": "session_teleported", "to": "mars"}))
            .unwrap();
    assert_eq!(change.event, SessionsEvent::Unknown);
    let change: StateChange =
        serde_json::from_value(json!({"rev": 7, "kind": "template_upserted", "template": {}}))
            .unwrap();
    assert_eq!(change.event, StateEvent::Unknown);
    let connectivity: Connectivity =
        serde_json::from_value(json!({"state": "sleeping", "until": 9})).unwrap();
    assert_eq!(connectivity, Connectivity::Unknown);
}

#[test]
fn unknown_error_kind_still_fails_only_its_call() {
    let line = r#"{"jsonrpc":"2.0","id":5,"error":{"code":-32060,"message":"over quota","data":{"kind":"quota_exceeded","limit":3}}}"#;
    let Ok(Message::Response(response)) = Message::parse(line) else {
        panic!("expected a response");
    };
    assert_eq!(response.id, Some(RpcId::Number(5)));
    let err = response.into_typed::<dm::SessionPrompt>().unwrap_err();
    assert_eq!(err.code, -32060);
    assert_eq!(err.message, "over quota");
    assert_eq!(err.data, Some(ErrorData::Unknown));
}

#[test]
fn bad_result_fails_only_its_own_id() {
    let good = r#"{"jsonrpc":"2.0","id":1,"result":{"outcome":"queued","queue_id":"01HZY3K6W8Q9X2V4N7B5C1D3ED"}}"#;
    let bad = r#"{"jsonrpc":"2.0","id":2,"result":{"outcome":"queued","queue_id":42}}"#;
    let decode = |line: &str| match Message::parse(line).unwrap() {
        Message::Response(r) => (r.id.clone(), r.into_typed::<dm::SessionPrompt>()),
        other => panic!("expected a response, got {other:?}"),
    };
    let (id, result) = decode(good);
    assert_eq!(id, Some(RpcId::Number(1)));
    assert!(matches!(result, Ok(PromptResult::Queued { .. })));
    let (id, result) = decode(bad);
    assert_eq!(id, Some(RpcId::Number(2)));
    assert_eq!(result.unwrap_err().data, Some(ErrorData::InvalidResponse));
}

#[test]
fn newer_result_variants_and_fields_still_decode() {
    let value = json!({"outcome": "deferred", "until": 99});
    assert_eq!(
        serde_json::from_value::<PromptResult>(value).unwrap(),
        PromptResult::Unknown
    );
    let hello: HelloResult = serde_json::from_value(json!({
        "daemon_version": "0.2.0",
        "protocol_version": {"major": 1, "minor": 4},
        "daemon_id": DID,
        "capabilities": ["jobs", "terminals"],
        "uptime_s": 12
    }))
    .unwrap();
    assert!(
        hello
            .protocol_version
            .is_compatible_with(AGENTD_PROTOCOL_VERSION)
    );
    assert!(hello.capabilities.contains(&Capability::JOBS));
    assert!(hello.capabilities.contains(&Capability::new("terminals")));

    // A newer daemon's send-now outcome, first-prompt outcome or `not_found` resource.
    assert_eq!(
        serde_json::from_value::<SendNowResult>(json!({"outcome": "deferred"})).unwrap(),
        SendNowResult::Unknown
    );
    let created: SessionCreateResult = serde_json::from_value(json!({
        "session": summary_json(),
        "prompt": {"outcome": "scheduled", "at": 5}
    }))
    .unwrap();
    assert_eq!(created.prompt, Some(PromptResult::Unknown));
    let error: RpcError = serde_json::from_value(json!({
        "code": -32007, "message": "x",
        "data": {"kind": "not_found", "resource": "terminal", "id": "t1"}
    }))
    .unwrap();
    assert_eq!(
        error.data,
        Some(ErrorData::NotFound {
            resource: Resource::Unknown,
            id: "t1".into()
        })
    );
    // Capabilities this daemon advertises are plain strings.
    assert_eq!(
        serde_json::to_value(&Capability::SANDBOX_APPROVALS).unwrap(),
        json!("sandbox_approvals")
    );
}

#[test]
fn unknown_notifications_and_methods_are_typed_errors() {
    let err = AgentdNotification::from_parts("session.sparkles", json!({})).unwrap_err();
    assert_eq!(err.data, Some(ErrorData::MethodNotFound));
    let err = AgentdRequest::from_parts("session.teleport", json!({})).unwrap_err();
    assert_eq!(err.data, Some(ErrorData::MethodNotFound));
    let err = AgentdRequest::from_parts("session.cancel", json!({"id": SID})).unwrap_err();
    assert_eq!(err.data, Some(ErrorData::InvalidParams));
    let ok = UiRequest::from_parts("agents.list", Value::Null).unwrap();
    assert_eq!(ok.method(), "agents.list");
}

// ---------------------------------------------------------------------------------------------
// Method tables
// ---------------------------------------------------------------------------------------------

#[test]
fn method_names_are_unique() {
    for names in [UiRequest::METHODS, UiNotification::NOTIFICATIONS] {
        let set: BTreeSet<_> = names.iter().collect();
        assert_eq!(set.len(), names.len(), "duplicate method name in {names:?}");
    }
}

#[test]
fn request_and_job_items_round_trip() {
    let item = |id: &str, body: ItemBody| TimelineItem {
        id: ItemId::new(id),
        seq_from: Seq(7),
        seq_to: Seq(9),
        actor: Actor::Root,
        ts: UnixMillis(1_000),
        streaming: false,
        body,
    };
    let job = Job {
        id: "01HZY3K6W8Q9X2V4N7B5C1D3EC".parse().unwrap(),
        name: "build".into(),
        command: "make".into(),
        cwd: RemotePath::new("~/src/app"),
        machine_id: None,
        workspace_id: None,
        status: JobStatus::Running,
        exit: None,
        signal: None,
        started_at: UnixMillis(1_000),
        ended_at: None,
        timeout_s: None,
        log_path: RemotePath::new("~/src/app/.controlroom/jobs/build.log"),
        lines: 0,
        follow: None,
    };
    for original in [
        item("req-item", ItemBody::Request { request: request() }),
        item("job-item", ItemBody::Job { job }),
    ] {
        let value = serde_json::to_value(&original).unwrap();
        assert_eq!(value["id"], json!(original.id.as_str()));
        let back: TimelineItem = serde_json::from_value(value).unwrap();
        assert_eq!(back, original);
    }
}
