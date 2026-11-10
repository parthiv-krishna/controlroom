//! The `ctl` timeline renderer against golden output, from synthetic ops.
//!
//! Regenerate the golden files with `UPDATE_GOLDEN=1 cargo test -p controlroom-agentd --test
//! ctl_render` and review the diff.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use controlroom_agentd::ctl::render::Renderer;
use controlroom_protocol::ids::{DaemonId, ItemId, JobId, RequestId, SessionId, TodoId};
use controlroom_protocol::session::{
    Author, ContentPart, Job, JobStatus, OptionStyle, PeerRef, Request, RequestAnswer, RequestKind,
    RequestOption, RequestState, Response, RunOutcome, SessionState, Todo, TodoStatus,
};
use controlroom_protocol::timeline::{
    Actor, CompactionPhase, CompactionReason, ItemBody, MessageDirection, MessageOutcome,
    MessageRole, NoticeLevel, RetryPhase, RunState, RunTrigger, TimelineItem, TimelineOp,
    TokenUsage, ToolKind, ToolOutput, ToolStatus, UsageScope,
};
use controlroom_protocol::{Seq, UnixMillis};

const ULID: &str = "01HZY3K4M5N6P7Q8R9S0T1V2W3";

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn check_golden(name: &str, actual: &str) {
    let path = golden(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; run with UPDATE_GOLDEN=1", path.display()));
    assert_eq!(actual, expected, "golden {name} differs");
}

fn item(id: &str, seq: u64, streaming: bool, body: ItemBody) -> TimelineOp {
    TimelineOp::Upsert {
        item: Box::new(TimelineItem {
            id: ItemId::new(id),
            seq_from: Seq(seq),
            seq_to: Seq(seq),
            actor: Actor::Root,
            ts: UnixMillis(1_700_000_000_000 + seq as i64),
            streaming,
            body,
        }),
    }
}

fn append(id: &str, seq: u64, text: &str) -> TimelineOp {
    TimelineOp::AppendText {
        id: ItemId::new(id),
        text: text.into(),
        seq_from: Seq(seq),
        seq_to: Seq(seq),
    }
}

fn end(id: &str, seq: u64) -> TimelineOp {
    TimelineOp::End {
        id: ItemId::new(id),
        seq_from: Seq(seq),
        seq_to: Seq(seq),
    }
}

fn run(state: RunState, outcome: Option<RunOutcome>, error: Option<&str>) -> ItemBody {
    ItemBody::Run {
        trigger: RunTrigger::User { device: None },
        state,
        outcome,
        error: error.map(Into::into),
    }
}

fn message(role: MessageRole, text: &str) -> ItemBody {
    ItemBody::Message {
        role,
        content: vec![ContentPart::text(text)],
    }
}

fn tool(status: ToolStatus, output: Option<ToolOutput>, exit: Option<i32>) -> ItemBody {
    ItemBody::ToolCall {
        tool_call_id: "call_1".into(),
        parent_tool_call_id: None,
        name: "bash".into(),
        title: "Run cargo test".into(),
        tool_kind: ToolKind::Execute,
        input: Some("cargo test".into()),
        status,
        output,
        exit,
        duration_ms: exit.map(|_| 1250),
    }
}

fn request(state: RequestState, answer: Option<&str>) -> ItemBody {
    ItemBody::Request {
        request: Request {
            id: RequestId::new("req-1"),
            kind: RequestKind::Sandbox,
            blocking: true,
            state,
            title: "Run git push origin main?".into(),
            detail: None,
            body: None,
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
            questions: Vec::new(),
            answer: answer.map(|label| RequestAnswer {
                response: Response::Choice {
                    option_id: "allow".into(),
                    reason: None,
                },
                label: label.into(),
                by: None,
                at: UnixMillis(1),
            }),
            origin_child: None,
            created_at: UnixMillis(1),
        },
    }
}

fn todo(text: &str, status: TodoStatus) -> Todo {
    Todo {
        id: ULID.parse::<TodoId>().unwrap(),
        text: text.into(),
        status,
        reason: None,
        updated_by: Author::Agent,
        updated_at: UnixMillis(1),
    }
}

fn peer(name: &str) -> PeerRef {
    PeerRef {
        daemon_id: ULID.parse::<DaemonId>().unwrap(),
        session_id: ULID.parse::<SessionId>().unwrap(),
        name: name.into(),
    }
}

/// A session touching every item kind, with streaming interleaved across items.
fn session_ops() -> Vec<TimelineOp> {
    vec![
        item(
            "s1",
            1,
            false,
            ItemBody::SessionInfo {
                title: None,
                model: Some("mock-model".into()),
                thinking: Some("medium".into()),
                sandbox: None,
            },
        ),
        item("r1", 2, false, run(RunState::Running, None, None)),
        item(
            "m1",
            2,
            false,
            message(MessageRole::User, "Run the tests and push."),
        ),
        item(
            "t1",
            3,
            true,
            ItemBody::Thinking {
                text: "Need to".into(),
            },
        ),
        append("t1", 4, " run them first."),
        end("t1", 5),
        item("a1", 6, true, message(MessageRole::Agent, "")),
        append("a1", 6, "Running the "),
        append("a1", 7, "tests now.\nThis takes a moment."),
        item("c1", 8, true, tool(ToolStatus::Running, None, None)),
        append("c1", 9, "running 3 tests\ntest a ... ok\n"),
        append("c1", 10, "test b ... ok\n"),
        item(
            "c1",
            11,
            false,
            tool(
                ToolStatus::Completed,
                Some(ToolOutput::Terminal {
                    text: "running 3 tests\ntest a ... ok\ntest b ... ok\ntest c ... ok\n".into(),
                }),
                Some(0),
            ),
        ),
        end("a1", 12),
        item(
            "td",
            13,
            false,
            ItemBody::Todo {
                items: vec![
                    todo("run the tests", TodoStatus::Done),
                    todo("push", TodoStatus::InProgress),
                ],
            },
        ),
        item("q1", 14, false, request(RequestState::Pending, None)),
        item(
            "q1",
            15,
            false,
            request(RequestState::Answered, Some("Allowed once")),
        ),
        item(
            "j1",
            16,
            false,
            ItemBody::Job {
                job: Job {
                    id: ULID.parse::<JobId>().unwrap(),
                    name: "build".into(),
                    command: "make".into(),
                    cwd: "/home/alice/project".into(),
                    machine_id: None,
                    workspace_id: None,
                    status: JobStatus::Failed,
                    exit: Some(2),
                    signal: None,
                    started_at: UnixMillis(1),
                    ended_at: Some(UnixMillis(2)),
                    timeout_s: None,
                    log_path: "/home/alice/project/.controlroom/jobs/j1.log".into(),
                    lines: 10,
                    follow: None,
                },
            },
        ),
        item(
            "sub",
            17,
            false,
            ItemBody::Subagent {
                child: peer("reviewer"),
                title: "Review the diff".into(),
                agent: "pi".into(),
                state: SessionState::Idle,
                outcome: Some(RunOutcome::Completed),
                usage: None,
                result: None,
            },
        ),
        item(
            "msg",
            18,
            false,
            ItemBody::SessionMessage {
                message_id: ULID
                    .parse::<controlroom_protocol::ids::MessageId>()
                    .unwrap(),
                direction: MessageDirection::Sent,
                peer: peer("backend"),
                text: "API ready?".into(),
                reply_to: None,
                expects_reply: true,
                outcome: Some(MessageOutcome::Delivered),
                late: false,
            },
        ),
        item(
            "rt",
            19,
            false,
            ItemBody::Retry {
                attempt: 1,
                max_attempts: Some(3),
                phase: RetryPhase::Waiting,
                delay_ms: Some(2000),
                error: Some("Connection error.".into()),
            },
        ),
        item(
            "cp",
            20,
            false,
            ItemBody::Compaction {
                reason: CompactionReason::Threshold,
                phase: CompactionPhase::Done,
                tokens_before: Some(120_000),
                tokens_after: Some(30_000),
                error: None,
            },
        ),
        item(
            "u1",
            21,
            false,
            ItemBody::Usage {
                scope: UsageScope::Turn,
                tokens: TokenUsage {
                    input: 1200,
                    output: 340,
                    cache_read: 0,
                    cache_write: 0,
                },
                cost: Some(0.0123),
                context: None,
            },
        ),
        item(
            "n1",
            22,
            false,
            ItemBody::Notice {
                level: NoticeLevel::Warning,
                text: "extension reloaded".into(),
            },
        ),
        item(
            "e1",
            23,
            false,
            ItemBody::Error {
                text: "extension failed".into(),
            },
        ),
        item("x1", 24, false, ItemBody::Unknown),
        TimelineOp::Unknown,
        item(
            "r1",
            25,
            false,
            run(
                RunState::Settled,
                Some(RunOutcome::Failed),
                Some("push rejected"),
            ),
        ),
    ]
}

#[test]
fn renders_a_session() {
    let mut renderer = Renderer::new();
    let mut out = String::new();
    for op in session_ops() {
        renderer.apply(&op);
        out.push_str(&renderer.take());
    }
    out.push_str(&renderer.finish());
    check_golden("ctl-render.txt", &out);
}

#[test]
fn rendering_does_not_depend_on_batching() {
    let mut all_at_once = Renderer::new();
    for op in session_ops() {
        all_at_once.apply(&op);
    }
    let mut one_by_one = Renderer::new();
    let mut out = String::new();
    for op in session_ops() {
        one_by_one.apply(&op);
        out.push_str(&one_by_one.take());
    }
    out.push_str(&one_by_one.finish());
    assert_eq!(all_at_once.finish(), out);
}

#[test]
fn ctl_render_reads_jsonl() {
    let ops = [
        item("a1", 1, true, message(MessageRole::Agent, "Hel")),
        append("a1", 2, "lo"),
        end("a1", 3),
    ];
    let mut input = String::new();
    // A session.events notification, its params, and a bare op are all accepted.
    input.push_str(
        &serde_json::json!({"jsonrpc": "2.0", "method": "session.events", "params": {
            "subscription_id": ULID, "id": ULID, "head_seq": 1, "ops": [&ops[0]]}})
        .to_string(),
    );
    input.push('\n');
    input.push_str(
        &serde_json::json!({"subscription_id": ULID, "id": ULID, "head_seq": 2, "ops": [&ops[1]]})
            .to_string(),
    );
    input.push('\n');
    input.push_str(&serde_json::to_string(&ops[2]).unwrap());
    input.push('\n');

    let mut child = Command::new(env!("CARGO_BIN_EXE_controlroom-agentd"))
        .args(["ctl", "render"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ctl render");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "agent: Hello\n");

    let bad = Command::new(env!("CARGO_BIN_EXE_controlroom-agentd"))
        .args(["ctl", "render", "/nonexistent/ops.jsonl"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("controlroom-agentd ctl:"));
}
