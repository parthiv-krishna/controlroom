//! Wire-shape tests: golden JSON for representative messages, serde round-trips, forward
//! compatibility (unknown variants and fields, a bad result for one id), and the method tables.

use std::collections::BTreeSet;

use controlroom_protocol::daemon::methods as dm;
use controlroom_protocol::daemon::notifications as dm_notes;
use controlroom_protocol::daemon::*;
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
        cwd: "/home/alice/src/app".into(),
        agent: "pi".into(),
        created_at: UnixMillis(1_000),
        updated_at: UnixMillis(2_000),
        closed_at: None,
        parent_id: None,
        spawned_by: None,
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
        sandbox: SandboxInfo {
            level: SandboxLevel::Strict,
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
        "cwd": "/home/alice/src/app",
        "agent": "pi",
        "created_at": 1000,
        "updated_at": 2000,
        "closed_at": null,
        "parent_id": null,
        "spawned_by": null,
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
        "live_work": {"jobs": 1, "children": 0, "asks": 0},
        "options": [{
            "id": "thinking",
            "label": "Thinking",
            "kind": "select",
            "value": "medium",
            "choices": [{"value": "medium", "label": "Medium"}]
        }],
        "sandbox": {"level": "strict", "grants": []},
        "resumable": true
    })
}

// ---------------------------------------------------------------------------------------------
// Golden messages
// ---------------------------------------------------------------------------------------------

#[test]
fn golden_daemon_hello_request() {
    let request = RpcRequest::new::<dm::Hello>(
        1,
        HelloParams {
            protocol_version: DAEMON_PROTOCOL_VERSION,
            client_name: "controlroom-web 0.1.0".into(),
            controller_id: CID.parse().unwrap(),
            claim_broker: true,
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
                "controller_id": CID,
                "claim_broker": true
            }
        }),
    );
}

#[test]
fn golden_machine_events_notification() {
    let note = RpcNotification::new::<dm_notes::MachineEvents>(MachineEventsParams {
        subscription_id: sub(),
        changes: vec![
            MachineChange {
                rev: Rev(41),
                event: MachineEvent::SessionUpserted {
                    session: Box::new(summary()),
                },
            },
            MachineChange {
                rev: Rev(42),
                event: MachineEvent::SessionRemoved { id: sid() },
            },
        ],
    });
    golden(
        &note,
        json!({
            "jsonrpc": "2.0",
            "method": "machine.events",
            "params": {
                "subscription_id": SUB,
                "changes": [
                    {"rev": 41, "kind": "session_upserted", "session": summary_json()},
                    {"rev": 42, "kind": "session_removed", "id": SID}
                ]
            }
        }),
    );
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
        OnMachine::new(
            MID.parse().unwrap(),
            RequestRespondParams {
                op_id: OpId::new("op-1"),
                id: sid(),
                request_id: RequestId::new("r1"),
                response: Response::Choice {
                    option_id: "deny".into(),
                    reason: Some("push to a branch".into()),
                },
                then: Some(vec![ContentPart::text("push to fix/flaky instead")]),
            },
        ),
    );
    golden(
        &request,
        json!({
            "jsonrpc": "2.0",
            "id": "c7",
            "method": "request.respond",
            "params": {
                "machine_id": MID,
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
        event: StateEvent::SessionRemoved {
            machine_id: MID.parse().unwrap(),
            session_id: sid(),
        },
    };
    golden(
        &change,
        json!({"rev": 3, "kind": "session_removed", "machine_id": MID, "session_id": SID}),
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
fn golden_outbox_ack() {
    golden(
        &OutboxAckParams {
            item: "01HZY3K6W8Q9X2V4N7B5C1D3EC".parse().unwrap(),
            outcome: OutboxOutcome::ConnectionLost {
                scope: LinkScope::Peer,
                since: UnixMillis(5_000),
            },
        },
        json!({
            "item": "01HZY3K6W8Q9X2V4N7B5C1D3EC",
            "outcome": {"outcome": "connection_lost", "scope": "peer", "since": 5000}
        }),
    );
}

#[test]
fn summaries_round_trip_inside_ui_machine_sessions() {
    let session = MachineSession {
        machine_id: MID.parse().unwrap(),
        stale: true,
        session: summary(),
    };
    let mut expected = summary_json();
    expected["machine_id"] = json!(MID);
    expected["stale"] = json!(true);
    golden(&session, expected);
}

#[test]
fn actors_are_root_or_a_child_id() {
    assert_eq!(serde_json::to_value(Actor::Root).unwrap(), json!("root"));
    assert_eq!(serde_json::to_value(Actor::Child(sid())).unwrap(), json!(SID));
    assert_eq!(serde_json::from_value::<Actor>(json!(SID)).unwrap(), Actor::Child(sid()));
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
    newer["requests"][0]["kind"] = json!("privileged");
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
    let change: MachineChange =
        serde_json::from_value(json!({"rev": 7, "kind": "session_teleported", "to": "mars"}))
            .unwrap();
    assert_eq!(change.event, MachineEvent::Unknown);
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
        "broker": null,
        "uptime_s": 12
    }))
    .unwrap();
    assert!(hello.protocol_version.is_compatible_with(DAEMON_PROTOCOL_VERSION));
    assert!(hello.capabilities.contains(&Capability::JOBS));
    assert!(hello.capabilities.contains(&Capability::new("terminals")));
}

#[test]
fn unknown_notifications_and_methods_are_typed_errors() {
    let err = DaemonNotification::from_parts("session.sparkles", json!({})).unwrap_err();
    assert_eq!(err.data, Some(ErrorData::MethodNotFound));
    let err = DaemonRequest::from_parts("session.teleport", json!({})).unwrap_err();
    assert_eq!(err.data, Some(ErrorData::MethodNotFound));
    let err = DaemonRequest::from_parts("session.cancel", json!({"id": SID})).unwrap_err();
    assert_eq!(err.data, Some(ErrorData::InvalidParams));
    let ok = DaemonRequest::from_parts("agents.list", Value::Null).unwrap();
    assert_eq!(ok.method(), "agents.list");
}

// ---------------------------------------------------------------------------------------------
// Method tables
// ---------------------------------------------------------------------------------------------

#[test]
fn method_names_are_unique() {
    for names in [
        DaemonRequest::METHODS,
        DaemonNotification::NOTIFICATIONS,
        UiRequest::METHODS,
        UiNotification::NOTIFICATIONS,
    ] {
        let set: BTreeSet<_> = names.iter().collect();
        assert_eq!(set.len(), names.len(), "duplicate method name in {names:?}");
    }
}

#[test]
fn ui_passes_through_every_session_scoped_daemon_method() {
    // Daemon methods that are connection- or broker-level and never passed through.
    let daemon_only = [
        "hello",
        "machine.subscribe",
        "unsubscribe",
        "outbox.subscribe",
        "outbox.ack",
        "inbox.deliver",
    ];
    let ui: BTreeSet<_> = UiRequest::METHODS.iter().copied().collect();
    for method in DaemonRequest::METHODS {
        if !daemon_only.contains(method) {
            assert!(ui.contains(method), "{method} is not passed through by the UI API");
        }
    }
}
