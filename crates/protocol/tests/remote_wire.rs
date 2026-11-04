//! Wire-shape tests for the remote-tools APIs (host, agent daemon, the agent API's remote tools
//! and the UI API): golden JSON, forward compatibility (unknown variants and fields, a bad
//! result for one id) and the method tables.

use std::collections::BTreeSet;

use controlroom_protocol::agent::{self, AgentNotification, AgentRequest};
use controlroom_protocol::agentd::{
    self, AgentdNotification, AgentdRequest, ClientRole, FollowFrom, HelloParams, InitialPrompt,
};
use controlroom_protocol::host::{
    self, FsEntry, FsEntryKind, HostNotification, HostOp, HostRequest, HostResult,
};
use controlroom_protocol::ids::{
    CallId, Epoch, HostOpId, JobId, MachineId, OpId, ProjectId, SessionId, ShellId, SubscriptionId,
    WorkspaceId,
};
use controlroom_protocol::session::{
    Attention, AttentionRollup, Availability, ContentPart, LiveWork, PromptOrigin, QueueStatus,
    RequestBody, RequestKind, RequestState, RunOutcome, SandboxInfo, SandboxLevel, SandboxSpec,
    SessionState, SessionSummary, Shell, ShellState, Workspace, WorkspaceSpec,
};
use controlroom_protocol::timeline::InjectOrigin;
use controlroom_protocol::ui::{self, Connectivity, StateCursor};
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
const WID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3EB";
const JID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3EC";
const EPOCH: &str = "01HZY3K6W8Q9X2V4N7B5C1D3ED";
const PID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3EE";

fn sid() -> SessionId {
    SID.parse().unwrap()
}

fn mid() -> MachineId {
    MID.parse().unwrap()
}

fn sub() -> SubscriptionId {
    SUB.parse().unwrap()
}

fn wid() -> WorkspaceId {
    WID.parse().unwrap()
}

fn connected(since: i64) -> Availability {
    Availability {
        connectivity: Connectivity::Connected,
        since: UnixMillis(since),
    }
}

fn workspace() -> Workspace {
    Workspace {
        id: wid(),
        machine_id: mid(),
        path: "~/src/api".into(),
        root: Some("/home/alice/src/api".into()),
        primary: true,
        added_at: UnixMillis(1_000),
        availability: connected(900),
    }
}

fn workspace_json() -> Value {
    json!({
        "id": WID,
        "machine_id": MID,
        "path": "~/src/api",
        "root": "/home/alice/src/api",
        "primary": true,
        "added_at": 1000,
        "availability": {"connectivity": {"state": "connected"}, "since": 900}
    })
}

fn shell() -> Shell {
    Shell {
        name: "devbox".into(),
        workspace_id: wid(),
        cwd: "/home/alice/src/api/src".into(),
        state: ShellState::Running {
            call_id: CallId::new("call-7"),
        },
        reset_pending: false,
        foreground: Some("cargo".into()),
    }
}

fn shell_json() -> Value {
    json!({
        "name": "devbox",
        "workspace_id": WID,
        "cwd": "/home/alice/src/api/src",
        "state": {"state": "running", "call_id": "call-7"},
        "reset_pending": false,
        "foreground": "cargo"
    })
}

fn summary() -> SessionSummary {
    SessionSummary {
        id: sid(),
        name: Some("api tests".into()),
        title: None,
        workspaces: vec![workspace()],
        shells: vec![shell()],
        created_at: UnixMillis(1_000),
        updated_at: UnixMillis(2_000),
        closed_at: None,
        parent_id: None,
        state: SessionState::Working,
        last_outcome: Some(RunOutcome::Completed),
        last_error: None,
        read_seq: Seq(10),
        last_turn_end_seq: Seq(12),
        unread: false,
        preview: "cargo test".into(),
        attention: Some(Attention::Unread),
        rollup: AttentionRollup::default(),
        context: None,
        queue: QueueStatus::default(),
        requests: vec![],
        todo: None,
        live_work: LiveWork {
            host_calls: 1,
            ..LiveWork::default()
        },
        options: vec![],
        agent_live: true,
        sandbox: SandboxInfo {
            level: SandboxLevel::Strict,
            default_level: SandboxLevel::Strict,
            explicit: false,
            looser_than_default: false,
            grants: vec![],
        },
        resumable: true,
    }
}

fn summary_json() -> Value {
    json!({
        "id": SID,
        "name": "api tests",
        "title": null,
        "workspaces": [workspace_json()],
        "shells": [shell_json()],
        "created_at": 1000,
        "updated_at": 2000,
        "closed_at": null,
        "parent_id": null,
        "state": "working",
        "last_outcome": "completed",
        "last_error": null,
        "read_seq": 10,
        "last_turn_end_seq": 12,
        "unread": false,
        "preview": "cargo test",
        "attention": "unread",
        "rollup": {"approvals": 0, "questions": 0, "problems": 0, "unread": 0},
        "context": null,
        "queue": {"len": 0, "paused": false, "auto_send": false, "send_now_pending": false},
        "requests": [],
        "todo": null,
        "live_work": {"jobs": 0, "children": 0, "asks": 0, "host_calls": 1},
        "options": [],
        "agent_live": true,
        "sandbox": {
            "level": "strict",
            "default_level": "strict",
            "explicit": false,
            "looser_than_default": false,
            "grants": []
        },
        "resumable": true
    })
}

fn bash_result() -> HostResult {
    HostResult::Bash {
        exit_code: Some(0),
        signal: None,
        cwd: "/home/alice/src/api".into(),
        output: "ok\n".into(),
        truncated: false,
        log_path: "/home/alice/.local/state/controlroom/calls/c1.log".into(),
        timed_out: false,
        cancelled: false,
        waited_for_input: false,
        shell_reset: None,
        duration_ms: 812,
    }
}

fn bash_result_json() -> Value {
    json!({
        "op": "bash",
        "exit_code": 0,
        "signal": null,
        "cwd": "/home/alice/src/api",
        "output": "ok\n",
        "truncated": false,
        "log_path": "/home/alice/.local/state/controlroom/calls/c1.log",
        "timed_out": false,
        "cancelled": false,
        "waited_for_input": false,
        "shell_reset": null,
        "duration_ms": 812
    })
}

// ---------------------------------------------------------------------------------------------
// Host API
// ---------------------------------------------------------------------------------------------

#[test]
fn golden_host_hello() {
    let request = RpcRequest::new::<host::methods::Hello>(
        1,
        HelloParams {
            protocol_version: HOST_PROTOCOL_VERSION,
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
    golden(
        &host::HostHelloResult {
            host_version: "0.1.0".into(),
            protocol_version: HOST_PROTOCOL_VERSION,
            daemon_id: DID.parse().unwrap(),
            epoch: EPOCH.parse::<Epoch>().unwrap(),
            capabilities: vec![Capability::SHELLS, Capability::PTY, Capability::JOBS],
        },
        json!({
            "host_version": "0.1.0",
            "protocol_version": {"major": 1, "minor": 0},
            "daemon_id": DID,
            "epoch": EPOCH,
            "capabilities": ["shells", "pty", "jobs"]
        }),
    );
}

#[test]
fn golden_workspace_open() {
    golden(
        &host::WorkspaceOpenParams {
            workspace_id: wid(),
            session_id: sid(),
            path: "~/src/api".into(),
            sandbox: SandboxSpec {
                level: SandboxLevel::Strict,
                grants: vec![],
                domains: vec!["example.com".into()],
                mediated: vec![],
            },
        },
        json!({
            "workspace_id": WID,
            "session_id": SID,
            "path": "~/src/api",
            "sandbox": {"level": "strict", "grants": [], "domains": ["example.com"], "mediated": []}
        }),
    );
    golden(
        &host::WorkspaceOpenResult {
            root: "/home/alice/src/api".into(),
            home: "/home/alice".into(),
            context_files: vec![host::ContextFile {
                path: "/home/alice/src/AGENTS.md".into(),
                content: "Run cargo test.".into(),
            }],
            skills: vec![host::Skill {
                name: "release".into(),
                description: "Cut a release.".into(),
                path: "/home/alice/src/api/.pi/skills/release/SKILL.md".into(),
            }],
        },
        json!({
            "root": "/home/alice/src/api",
            "home": "/home/alice",
            "context_files": [{"path": "/home/alice/src/AGENTS.md", "content": "Run cargo test."}],
            "skills": [{
                "name": "release",
                "description": "Cut a release.",
                "path": "/home/alice/src/api/.pi/skills/release/SKILL.md"
            }]
        }),
    );
}

#[test]
fn golden_host_shells() {
    let id = ShellId::of(sid(), "devbox");
    assert_eq!(id.as_str(), format!("{SID}/devbox"));
    let host_shell = host::HostShell {
        shell_id: id,
        shell: shell(),
    };
    let mut expected = shell_json();
    expected["shell_id"] = json!(format!("{SID}/devbox"));
    golden(&host_shell, expected.clone());
    golden(
        &RpcNotification::new::<host::notifications::ShellChanged>(host::ShellChangedParams {
            shell: host_shell,
        }),
        json!({"jsonrpc": "2.0", "method": "shell.changed", "params": {"shell": expected}}),
    );
    let open: host::ShellOpenParams =
        serde_json::from_value(json!({"workspace_id": WID, "name": "devbox-2"})).unwrap();
    assert_eq!(open.cwd, None);
}

#[test]
fn golden_tool_run_bash() {
    let request = RpcRequest::new::<host::methods::ToolRun>(
        7,
        host::ToolRunParams {
            call_id: CallId::new("toolu_01"),
            workspace_id: wid(),
            op: HostOp::Bash {
                shell: "devbox".into(),
                command: "cargo test".into(),
                timeout_s: None,
            },
        },
    );
    golden(
        &request,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tool.run",
            "params": {
                "call_id": "toolu_01",
                "workspace_id": WID,
                "op": {"op": "bash", "shell": "devbox", "command": "cargo test"}
            }
        }),
    );
    golden(&bash_result(), bash_result_json());
    let line =
        serde_json::to_string(&RpcResponse::success(RpcId::Number(7), bash_result())).unwrap();
    let Ok(Message::Response(response)) = Message::parse(&line) else {
        panic!("expected a response");
    };
    assert_eq!(
        response.into_typed::<host::methods::ToolRun>().unwrap(),
        bash_result()
    );
}

#[test]
fn golden_file_ops_and_defaults() {
    // Optional params may be omitted, and decode to their defaults.
    let op: HostOp = serde_json::from_value(json!({"op": "grep", "pattern": "TODO"})).unwrap();
    assert_eq!(
        op,
        HostOp::Grep {
            pattern: "TODO".into(),
            path: None,
            glob: None,
            ignore_case: false,
            literal: false,
            context: None,
            limit: None,
        }
    );
    let write: HostOp = serde_json::from_value(json!({
        "op": "write", "path": "notes.md", "content": "aGk=", "encoding": "base64",
        "expect_sha256": "ab12"
    }))
    .unwrap();
    golden(
        &write,
        json!({
            "op": "write", "path": "notes.md", "content": "aGk=", "encoding": "base64",
            "expect_sha256": "ab12"
        }),
    );
    let plain: HostOp =
        serde_json::from_value(json!({"op": "write", "path": "a.txt", "content": "hi"})).unwrap();
    assert!(matches!(
        plain,
        HostOp::Write {
            encoding: host::ContentEncoding::Utf8,
            expect_sha256: None,
            ..
        }
    ));
    golden(
        &HostOp::Read {
            path: "src/main.rs".into(),
            offset: Some(500),
            limit: None,
        },
        json!({"op": "read", "path": "src/main.rs", "offset": 500}),
    );
    golden(
        &HostResult::Read {
            path: "/home/alice/src/api/src/main.rs".into(),
            size: 2048,
            content: host::ReadContent::Text {
                lines: vec!["fn main() {}".into()],
                start_line: 500,
                total_lines: 900,
                cut: Some(host::ReadCut::Lines),
                first_line_too_long: false,
            },
        },
        json!({
            "op": "read",
            "path": "/home/alice/src/api/src/main.rs",
            "size": 2048,
            "content": {
                "kind": "text",
                "lines": ["fn main() {}"],
                "start_line": 500,
                "total_lines": 900,
                "cut": "lines",
                "first_line_too_long": false
            }
        }),
    );
    golden(
        &HostResult::Ls {
            path: "/home/alice/src/api".into(),
            entries: vec![FsEntry {
                name: "src".into(),
                kind: FsEntryKind::Dir,
                size: None,
                mtime: None,
            }],
            total: 1,
        },
        json!({
            "op": "ls",
            "path": "/home/alice/src/api",
            "entries": [{"name": "src", "kind": "dir", "size": null, "mtime": null}],
            "total": 1
        }),
    );
}

#[test]
fn golden_job_events() {
    let events = host::JobsEventParams {
        subscription_id: sub(),
        events: vec![
            host::JobEvent {
                rev: Rev(4),
                at: UnixMillis(5_000),
                job_id: JID.parse().unwrap(),
                kind: host::JobEventKind::Progress {
                    lines: 14,
                    offset: 640,
                    matches: vec![8, 10],
                },
            },
            host::JobEvent {
                rev: Rev(5),
                at: UnixMillis(6_000),
                job_id: JID.parse().unwrap(),
                kind: host::JobEventKind::Exit {
                    exit_code: None,
                    signal: None,
                    timed_out: false,
                    lost: Some("host rebooted".into()),
                    lines: 14,
                    duration_ms: 10_200,
                },
            },
        ],
    };
    golden(
        &RpcNotification::new::<host::notifications::JobsEvent>(events),
        json!({
            "jsonrpc": "2.0",
            "method": "jobs.event",
            "params": {
                "subscription_id": SUB,
                "events": [
                    {"rev": 4, "at": 5000, "job_id": JID, "kind": "progress",
                     "lines": 14, "offset": 640, "matches": [8, 10]},
                    {"rev": 5, "at": 6000, "job_id": JID, "kind": "exit", "exit_code": null,
                     "signal": null, "timed_out": false, "lost": "host rebooted", "lines": 14,
                     "duration_ms": 10200}
                ]
            }
        }),
    );
    golden(
        &HostOp::JobFollow {
            job_id: JID.parse().unwrap(),
            pattern: Some("ERROR".into()),
            buffer_seconds: None,
            from: FollowFrom::Unseen,
        },
        json!({"op": "job_follow", "job_id": JID, "pattern": "ERROR", "from": "unseen"}),
    );
}

#[test]
fn golden_host_approval() {
    let approval = host::HostApproval {
        id: "01HZY3K6W8Q9X2V4N7B5C1D3EF".parse().unwrap(),
        session_id: sid(),
        workspace_id: wid(),
        title: "Run git push origin main?".into(),
        detail: None,
        body: RequestBody::GitPush {
            command: "git push origin main".into(),
            remote: "origin".into(),
            branch: "main".into(),
            force: false,
        },
        options: vec![],
        created_at: UnixMillis(3_000),
    };
    golden(
        &RpcNotification::new::<host::notifications::ApprovalRequest>(
            host::ApprovalRequestParams {
                subscription_id: sub(),
                approval,
            },
        ),
        json!({
            "jsonrpc": "2.0",
            "method": "approval.request",
            "params": {
                "subscription_id": SUB,
                "approval": {
                    "id": "01HZY3K6W8Q9X2V4N7B5C1D3EF",
                    "session_id": SID,
                    "workspace_id": WID,
                    "title": "Run git push origin main?",
                    "detail": null,
                    "body": {"kind": "git_push", "command": "git push origin main",
                             "remote": "origin", "branch": "main", "force": false},
                    "options": [],
                    "created_at": 3000
                }
            }
        }),
    );
    let closed: host::ApprovalClosedParams = serde_json::from_value(json!({
        "subscription_id": SUB, "approval_id": "01HZY3K6W8Q9X2V4N7B5C1D3EF", "state": "expired"
    }))
    .unwrap();
    assert_eq!(closed.state, RequestState::Expired);
}

#[test]
fn golden_host_errors() {
    for (data, wire) in [
        (
            ErrorData::Conflict {
                path: "/home/alice/src/api/a.rs".into(),
            },
            json!({"kind": "conflict", "path": "/home/alice/src/api/a.rs"}),
        ),
        (
            ErrorData::Lost { may_have_run: true },
            json!({"kind": "lost", "may_have_run": true}),
        ),
        (
            ErrorData::ShellGone {
                shell: "devbox".into(),
            },
            json!({"kind": "shell_gone", "shell": "devbox"}),
        ),
        (
            ErrorData::ConnectionLost {
                machine_id: mid(),
                since: UnixMillis(9),
            },
            json!({"kind": "connection_lost", "machine_id": MID, "since": 9}),
        ),
    ] {
        let err = RpcError::new(data, "x");
        let value = serde_json::to_value(&err).unwrap();
        assert_eq!(value["data"], wire);
        assert_eq!(value["code"], json!(err.code));
        let back: RpcError = serde_json::from_value(value).unwrap();
        assert_eq!(back, err);
    }
    let codes: BTreeSet<i32> = [
        ErrorData::Conflict { path: "a".into() },
        ErrorData::Lost {
            may_have_run: false,
        },
        ErrorData::ShellGone { shell: "s".into() },
        ErrorData::ConnectionLost {
            machine_id: mid(),
            since: UnixMillis(0),
        },
        ErrorData::ControllerConflict {
            controller_id: CID.parse().unwrap(),
            connected_since: UnixMillis(0),
        },
    ]
    .iter()
    .map(ErrorData::code)
    .collect();
    assert_eq!(codes.len(), 5, "error codes are distinct");
}

// ---------------------------------------------------------------------------------------------
// Agent daemon API
// ---------------------------------------------------------------------------------------------

#[test]
fn golden_agentd_session_create() {
    let request = RpcRequest::new::<agentd::methods::SessionCreate>(
        3,
        agentd::SessionCreateParams {
            op_id: OpId::new("op-1"),
            workspaces: vec![
                WorkspaceSpec {
                    machine_id: mid(),
                    path: "~/src/api".into(),
                },
                WorkspaceSpec {
                    machine_id: mid(),
                    path: "~/src/web".into(),
                },
            ],
            name: None,
            parent_id: None,
            sandbox_level: None,
            sandbox: None,
            prompt: Some(InitialPrompt {
                content: vec![ContentPart::text("Run the tests.")],
                origin: PromptOrigin::default(),
            }),
        },
    );
    golden(
        &request,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session.create",
            "params": {
                "op_id": "op-1",
                "workspaces": [
                    {"machine_id": MID, "path": "~/src/api"},
                    {"machine_id": MID, "path": "~/src/web"}
                ],
                "prompt": {
                    "content": [{"kind": "text", "text": "Run the tests."}],
                    "origin": {"kind": "user", "device": null}
                }
            }
        }),
    );
    golden(
        &agentd::SessionCreateResult {
            session: summary(),
            prompt: None,
        },
        json!({"session": summary_json(), "prompt": null}),
    );
}

#[test]
fn golden_sessions_events() {
    golden(
        &RpcNotification::new::<agentd::notifications::SessionsEvents>(
            agentd::SessionsEventsParams {
                subscription_id: sub(),
                changes: vec![
                    agentd::SessionsChange {
                        rev: Rev(8),
                        event: agentd::SessionsEvent::SessionUpserted {
                            session: Box::new(summary()),
                        },
                    },
                    agentd::SessionsChange {
                        rev: Rev(9),
                        event: agentd::SessionsEvent::SessionRemoved { id: sid() },
                    },
                ],
            },
        ),
        json!({
            "jsonrpc": "2.0",
            "method": "sessions.events",
            "params": {
                "subscription_id": SUB,
                "changes": [
                    {"rev": 8, "kind": "session_upserted", "session": summary_json()},
                    {"rev": 9, "kind": "session_removed", "id": SID}
                ]
            }
        }),
    );
}

#[test]
fn golden_hostops_round_trip() {
    let request = agentd::HostOpRequest {
        id: HostOpId::new("toolu_01"),
        machine_id: mid(),
        session_id: Some(sid()),
        request: HostRequest::ToolRun(host::ToolRunParams {
            call_id: CallId::new("toolu_01"),
            workspace_id: wid(),
            op: HostOp::Stat {
                path: "Cargo.toml".into(),
            },
        }),
    };
    golden(
        &RpcNotification::new::<agentd::notifications::HostOpsRequest>(
            agentd::HostOpsRequestParams {
                subscription_id: sub(),
                requests: vec![request.clone()],
            },
        ),
        json!({
            "jsonrpc": "2.0",
            "method": "hostops.request",
            "params": {
                "subscription_id": SUB,
                "requests": [{
                    "id": "toolu_01",
                    "machine_id": MID,
                    "session_id": SID,
                    "method": "tool.run",
                    "params": {
                        "call_id": "toolu_01",
                        "workspace_id": WID,
                        "op": {"op": "stat", "path": "Cargo.toml"}
                    }
                }]
            }
        }),
    );
    // The controller forwards the host's response verbatim, result or error.
    golden(
        &agentd::HostOpsRespondParams {
            id: HostOpId::new("toolu_01"),
            outcome: RpcOutcome::Result(serde_json::to_value(bash_result()).unwrap()),
        },
        json!({"id": "toolu_01", "result": bash_result_json()}),
    );
    let failed: agentd::HostOpsRespondParams = serde_json::from_value(json!({
        "id": "toolu_02",
        "error": {"code": -32020, "message": "changed", "data": {"kind": "conflict", "path": "/a"}}
    }))
    .unwrap();
    let RpcOutcome::Error(err) = failed.outcome else {
        panic!("expected an error");
    };
    assert_eq!(err.data, Some(ErrorData::Conflict { path: "/a".into() }));
    // Host notifications travel inside `hostops.notify`.
    golden(
        &agentd::HostOpsNotifyParams {
            machine_id: mid(),
            notification: HostNotification::ToolOutput(host::ToolOutputParams {
                call_id: CallId::new("toolu_01"),
                text: "tick 2\n".into(),
            }),
        },
        json!({
            "machine_id": MID,
            "notification": {
                "method": "tool.output",
                "params": {"call_id": "toolu_01", "text": "tick 2\n"}
            }
        }),
    );
    golden(
        &agentd::HostOpsMachinesParams {
            machines: vec![agentd::HostMachine {
                machine_id: mid(),
                name: "devbox".into(),
                availability: Availability {
                    connectivity: Connectivity::Unreachable,
                    since: UnixMillis(77),
                },
                epoch: Some(EPOCH.parse().unwrap()),
                grace_s: 600,
            }],
        },
        json!({
            "machines": [{
                "machine_id": MID,
                "name": "devbox",
                "availability": {"connectivity": {"state": "unreachable"}, "since": 77},
                "epoch": EPOCH,
                "grace_s": 600
            }]
        }),
    );
}

// ---------------------------------------------------------------------------------------------
// Agent API: remote tools
// ---------------------------------------------------------------------------------------------

#[test]
fn golden_host_call() {
    let request = RpcRequest::new::<agent::methods::HostCall>(
        11,
        agent::HostCallParams {
            call_id: CallId::new("toolu_09/read"),
            machine: "devbox".into(),
            op: HostOp::ReadBytes {
                path: "src/main.rs".into(),
                offset: None,
                length: None,
            },
        },
    );
    golden(
        &request,
        json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "host.call",
            "params": {
                "call_id": "toolu_09/read",
                "machine": "devbox",
                "op": {"op": "read_bytes", "path": "src/main.rs"}
            }
        }),
    );
    golden(
        &agent::HostCallResult {
            result: bash_result(),
            link_drops: 1,
        },
        json!({"result": bash_result_json(), "link_drops": 1}),
    );
}

#[test]
fn golden_session_info() {
    let info = agent::SessionInfo {
        workspaces: vec![agent::WorkspaceInfo {
            machine: "devbox".into(),
            path: "~/src/api".into(),
            root: Some("/home/alice/src/api".into()),
            home: Some("/home/alice".into()),
            primary: true,
            shell: "devbox".into(),
            availability: connected(900),
            context_files: vec![host::ContextFile {
                path: "/home/alice/src/api/AGENTS.md".into(),
                content: "Use cargo.".into(),
            }],
            skills: vec![],
        }],
        shells: vec![agent::AgentShell {
            machine: "devbox".into(),
            shell: shell(),
        }],
    };
    let mut agent_shell = shell_json();
    agent_shell["machine"] = json!("devbox");
    golden(
        &RpcNotification::new::<agent::notifications::SessionInfoChanged>(info),
        json!({
            "jsonrpc": "2.0",
            "method": "session.info",
            "params": {
                "workspaces": [{
                    "machine": "devbox",
                    "path": "~/src/api",
                    "root": "/home/alice/src/api",
                    "home": "/home/alice",
                    "primary": true,
                    "shell": "devbox",
                    "availability": {"connectivity": {"state": "connected"}, "since": 900},
                    "context_files": [{"path": "/home/alice/src/api/AGENTS.md", "content": "Use cargo."}],
                    "skills": []
                }],
                "shells": [agent_shell]
            }
        }),
    );
    // `session.info` is both a method and a notification of the agent API.
    assert!(AgentRequest::METHODS.contains(&"session.info"));
    assert!(AgentNotification::NOTIFICATIONS.contains(&"session.info"));
    let shell_new: agent::ShellNewParams = serde_json::from_value(
        json!({"call_id": "toolu_3", "name": "devbox-2", "machine": "devbox"}),
    )
    .unwrap();
    assert_eq!(shell_new.cwd, None);
}

#[test]
fn job_run_names_a_machine() {
    let run: agent::JobRunParams = serde_json::from_value(
        json!({"call_id": "toolu_4", "command": "make", "machine": "buildbox", "cwd": "web"}),
    )
    .unwrap();
    assert_eq!(run.machine.as_deref(), Some("buildbox"));
    // The old daemon's shape still decodes.
    let old: agent::JobRunParams =
        serde_json::from_value(json!({"call_id": "toolu_5", "command": "make"})).unwrap();
    assert_eq!(old.machine, None);
}

// ---------------------------------------------------------------------------------------------
// UI API
// ---------------------------------------------------------------------------------------------

#[test]
fn golden_ui_state_events() {
    let feed = ui::FeedSession {
        project_ids: vec![PID.parse::<ProjectId>().unwrap()],
        session: summary(),
    };
    let mut feed_json = summary_json();
    feed_json["project_ids"] = json!([PID]);
    golden(
        &RpcNotification::new::<ui::notifications::StateEvents>(ui::StateEventsParams {
            subscription_id: sub(),
            changes: vec![
                ui::StateChange {
                    rev: Rev(3),
                    event: ui::StateEvent::SessionUpserted {
                        session: Box::new(feed),
                    },
                },
                ui::StateChange {
                    rev: Rev(4),
                    event: ui::StateEvent::OverlapsChanged {
                        overlaps: vec![ui::Overlap {
                            machine_id: mid(),
                            path: "/home/alice/src/api".into(),
                            session_ids: vec![sid(), "01HZY3K6W8Q9X2V4N7B5C1D3F0".parse().unwrap()],
                        }],
                    },
                },
                ui::StateChange {
                    rev: Rev(5),
                    event: ui::StateEvent::ProjectUpserted {
                        project: ui::Project {
                            id: PID.parse().unwrap(),
                            name: "API".into(),
                            color: "#a78bfa".into(),
                        },
                    },
                },
            ],
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "state.events",
            "params": {
                "subscription_id": SUB,
                "changes": [
                    {"rev": 3, "kind": "session_upserted", "session": feed_json},
                    {"rev": 4, "kind": "overlaps_changed", "overlaps": [{
                        "machine_id": MID,
                        "path": "/home/alice/src/api",
                        "session_ids": [SID, "01HZY3K6W8Q9X2V4N7B5C1D3F0"]
                    }]},
                    {"rev": 5, "kind": "project_upserted",
                     "project": {"id": PID, "name": "API", "color": "#a78bfa"}}
                ]
            }
        }),
    );
    let result: ui::StateSubscribeResult = serde_json::from_value(json!({
        "subscription_id": SUB,
        "cursor": {"epoch": EPOCH, "rev": 5},
        "snapshot": null
    }))
    .unwrap();
    assert_eq!(
        result.cursor,
        StateCursor {
            epoch: EPOCH.parse().unwrap(),
            rev: Rev(5)
        }
    );
}

#[test]
fn golden_ui_session_create_and_links() {
    let create: ui::UiSessionCreateParams = serde_json::from_value(json!({
        "op_id": "op-2",
        "workspaces": [{"machine_id": MID, "path": "~/src/api"}],
        "task_ids": ["01HZY3K6W8Q9X2V4N7B5C1D3F1"]
    }))
    .unwrap();
    assert_eq!(create.create.workspaces.len(), 1);
    assert_eq!(create.task_ids.len(), 1);
    golden(
        &ui::TasksLinkParams {
            task_id: "01HZY3K6W8Q9X2V4N7B5C1D3F1".parse().unwrap(),
            session_id: sid(),
        },
        json!({"task_id": "01HZY3K6W8Q9X2V4N7B5C1D3F1", "session_id": SID}),
    );
    // Session commands carry no machine.
    let prompt = ui::UiRequest::from_parts(
        "session.prompt",
        json!({"op_id": "op-3", "id": SID, "content": [{"kind": "text", "text": "hi"}]}),
    )
    .unwrap();
    assert_eq!(prompt.method(), "session.prompt");
    let focus: ui::ClientFocusParams =
        serde_json::from_value(json!({"device_id": "d1", "session_id": SID, "visible": true}))
            .unwrap();
    assert_eq!(focus.session_id, Some(sid()));
    let doctor: ui::DoctorReportParams = serde_json::from_value(json!({})).unwrap();
    assert_eq!(doctor.machine_id, None);
}

// ---------------------------------------------------------------------------------------------
// Forward compatibility
// ---------------------------------------------------------------------------------------------

#[test]
fn unknown_variants_decode_as_unknown() {
    let op: HostOp = serde_json::from_value(json!({"op": "teleport", "to": "mars"})).unwrap();
    assert_eq!(op, HostOp::Unknown);
    let result: HostResult =
        serde_json::from_value(json!({"op": "teleport", "arrived": true})).unwrap();
    assert_eq!(result, HostResult::Unknown);
    let read: HostResult = serde_json::from_value(json!({
        "op": "read", "path": "/a", "size": 1, "content": {"kind": "hologram", "fps": 60}
    }))
    .unwrap();
    assert!(matches!(
        read,
        HostResult::Read {
            content: host::ReadContent::Unknown,
            ..
        }
    ));
    let cut: host::ReadCut = serde_json::from_value(json!("tokens")).unwrap();
    assert_eq!(cut, host::ReadCut::Unknown);
    let state: ShellState =
        serde_json::from_value(json!({"state": "sleeping", "until": 5})).unwrap();
    assert_eq!(state, ShellState::Unknown);
    let event: host::JobEvent = serde_json::from_value(
        json!({"rev": 9, "at": 1, "job_id": JID, "kind": "paused", "why": "load"}),
    )
    .unwrap();
    assert_eq!(event.kind, host::JobEventKind::Unknown);
    assert_eq!(event.rev, Rev(9));
    let killed: host::KilledBy = serde_json::from_value(json!("scheduler")).unwrap();
    assert_eq!(killed, host::KilledBy::Unknown);
    let availability: Availability = serde_json::from_value(
        json!({"connectivity": {"state": "hibernating"}, "since": 3, "extra": 1}),
    )
    .unwrap();
    assert_eq!(availability.connectivity, Connectivity::Unknown);
    let change: agentd::SessionsChange =
        serde_json::from_value(json!({"rev": 2, "kind": "session_archived", "id": SID})).unwrap();
    assert_eq!(change.event, agentd::SessionsEvent::Unknown);
    let change: ui::StateChange =
        serde_json::from_value(json!({"rev": 2, "kind": "workspace_moved"})).unwrap();
    assert_eq!(change.event, ui::StateEvent::Unknown);
    let kind: RequestKind = serde_json::from_value(json!("terminal_input")).unwrap();
    assert_eq!(kind, RequestKind::TerminalInput);
    let origin: InjectOrigin = serde_json::from_value(json!("tool_late")).unwrap();
    assert_eq!(origin, InjectOrigin::ToolLate);
    let body: RequestBody = serde_json::from_value(json!({
        "kind": "privileged", "machine": "devbox", "command": "sudo nixos-rebuild switch",
        "reason": "apply the config", "cwd": null
    }))
    .unwrap();
    assert!(matches!(body, RequestBody::Privileged { .. }));
    // A newer summary with fields this build doesn't know still decodes.
    let mut newer = summary_json();
    newer["workspaces"][0]["mounts"] = json!(["x"]);
    newer["shells"][0]["state"] = json!({"state": "paused"});
    newer["future"] = json!(true);
    let decoded: SessionSummary = serde_json::from_value(newer).unwrap();
    assert_eq!(decoded.shells[0].state, ShellState::Unknown);
    // The old daemon's live work (without host calls) still decodes.
    let old: LiveWork =
        serde_json::from_value(json!({"jobs": 1, "children": 0, "asks": 0})).unwrap();
    assert_eq!(old.host_calls, 0);
}

#[test]
fn bad_result_fails_only_its_own_id() {
    let good = format!(
        r#"{{"jsonrpc":"2.0","id":1,"result":{}}}"#,
        serde_json::to_string(&bash_result()).unwrap()
    );
    let bad = r#"{"jsonrpc":"2.0","id":2,"result":{"op":"bash","exit_code":"zero"}}"#;
    let decode = |line: &str| match Message::parse(line).unwrap() {
        Message::Response(r) => (r.id.clone(), r.into_typed::<host::methods::ToolRun>()),
        other => panic!("expected a response, got {other:?}"),
    };
    let (id, result) = decode(&good);
    assert_eq!(id, Some(RpcId::Number(1)));
    assert_eq!(result.unwrap(), bash_result());
    let (id, result) = decode(bad);
    assert_eq!(id, Some(RpcId::Number(2)));
    assert_eq!(result.unwrap_err().data, Some(ErrorData::InvalidResponse));

    // The same for the agent's host call, and for an unknown error kind from a newer host.
    let bad = r#"{"jsonrpc":"2.0","id":3,"result":{"result":{"op":"bash"},"link_drops":0}}"#;
    let Ok(Message::Response(response)) = Message::parse(bad) else {
        panic!("expected a response");
    };
    assert_eq!(response.id, Some(RpcId::Number(3)));
    assert_eq!(
        response
            .into_typed::<agent::methods::HostCall>()
            .unwrap_err()
            .data,
        Some(ErrorData::InvalidResponse)
    );
    let newer = r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32070,"message":"disk full","data":{"kind":"quota","free":0}}}"#;
    let Ok(Message::Response(response)) = Message::parse(newer) else {
        panic!("expected a response");
    };
    let err = response.into_typed::<host::methods::ToolRun>().unwrap_err();
    assert_eq!((err.code, err.data), (-32070, Some(ErrorData::Unknown)));
}

#[test]
fn unknown_methods_and_notifications_are_typed_errors() {
    let err = HostRequest::from_parts("tool.teleport", json!({})).unwrap_err();
    assert_eq!(err.data, Some(ErrorData::MethodNotFound));
    let err = HostRequest::from_parts("tool.run", json!({"call_id": "c"})).unwrap_err();
    assert_eq!(err.data, Some(ErrorData::InvalidParams));
    let err = HostNotification::from_parts("tool.sparkles", json!({})).unwrap_err();
    assert_eq!(err.data, Some(ErrorData::MethodNotFound));
    let err = AgentdRequest::from_parts("outbox.subscribe", json!({})).unwrap_err();
    assert_eq!(err.data, Some(ErrorData::MethodNotFound));
    let err = AgentdNotification::from_parts("machine.events", json!({})).unwrap_err();
    assert_eq!(err.data, Some(ErrorData::MethodNotFound));
    let err = AgentRequest::from_parts("host.teleport", json!({})).unwrap_err();
    assert_eq!(err.data, Some(ErrorData::MethodNotFound));
    let ok = HostRequest::from_parts("approvals.subscribe", Value::Null).unwrap();
    assert_eq!(ok.method(), "approvals.subscribe");
    // An unknown op is a valid call: the host answers it with `unsupported`.
    let run = HostRequest::from_parts(
        "tool.run",
        json!({"call_id": "c", "workspace_id": WID, "op": {"op": "teleport"}}),
    )
    .unwrap();
    assert!(matches!(
        run,
        HostRequest::ToolRun(host::ToolRunParams {
            op: HostOp::Unknown,
            ..
        })
    ));
}

// ---------------------------------------------------------------------------------------------
// Method tables
// ---------------------------------------------------------------------------------------------

#[test]
fn method_names_are_unique() {
    for names in [
        HostRequest::METHODS,
        HostNotification::NOTIFICATIONS,
        AgentdRequest::METHODS,
        AgentdNotification::NOTIFICATIONS,
        AgentRequest::METHODS,
        AgentNotification::NOTIFICATIONS,
        ui::UiRequest::METHODS,
        ui::UiNotification::NOTIFICATIONS,
    ] {
        let set: BTreeSet<_> = names.iter().collect();
        assert_eq!(set.len(), names.len(), "duplicate method name in {names:?}");
    }
}

#[test]
fn ui_passes_through_every_session_scoped_agentd_method() {
    // Agent daemon methods that are connection- or controller-level and never passed through.
    let agentd_only = [
        "hello",
        "sessions.subscribe",
        "hostops.subscribe",
        "hostops.respond",
        "hostops.notify",
        "hostops.machines",
        "doctor.report",
    ];
    let ui: BTreeSet<_> = ui::UiRequest::METHODS.iter().copied().collect();
    for method in AgentdRequest::METHODS {
        if !agentd_only.contains(method) {
            assert!(
                ui.contains(method),
                "{method} is not passed through by the UI API"
            );
        }
    }
    // `doctor.report` exists too, with its own target (a machine or the server).
    assert!(ui.contains("doctor.report"));
}

#[test]
fn job_ids_and_workspace_ids_are_validated() {
    assert!(serde_json::from_value::<WorkspaceId>(json!("not-a-ulid")).is_err());
    assert!(serde_json::from_value::<JobId>(json!(JID)).is_ok());
    // Call ids are opaque: pi's ids and step suffixes alike.
    let call: CallId = serde_json::from_value(json!("toolu_01ABC/write")).unwrap();
    assert_eq!(call.as_str(), "toolu_01ABC/write");
}
