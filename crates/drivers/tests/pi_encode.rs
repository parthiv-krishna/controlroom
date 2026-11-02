//! Encode round trips: an intent's lines, logged as `in` records, are recognized by the fold,
//! and pi's answers map back to `Accepted` / `Rejected` for the same tags.

mod support;

use controlroom_drivers::{
    Driver, DriverEvent, EncodeError, Intent, LineTag, NativeRef, PiDriver, StartMode,
};
use controlroom_protocol::daemon::OptionValue;
use controlroom_protocol::ids::{OpId, QueueId};
use controlroom_protocol::session::{
    ContentPart, QuestionAnswer, RequestKind, RequestState, Response, RunOutcome,
};
use controlroom_protocol::timeline::{InjectOrigin, ItemBody, RunTrigger, TimelineOp};
use controlroom_transcript::{Body, Record};
use serde_json::{Value, json};
use support::{in_line, out_line};

/// A driver plus a transcript cursor.
struct Session {
    driver: PiDriver,
    seq: u64,
}

impl Session {
    fn new() -> Self {
        let mut session = Self {
            driver: PiDriver::new(),
            seq: 0,
        };
        session.fold(Record {
            seq: 1,
            ts: 10,
            body: Body::Meta(Box::default()),
        });
        session
    }

    fn fold(&mut self, record: Record) -> controlroom_drivers::FoldOut {
        self.seq = record.seq;
        self.driver.fold(&record)
    }

    fn next(&mut self) -> u64 {
        self.seq + 1
    }

    /// Logs a line written to pi.
    fn write(&mut self, raw: &str) -> controlroom_drivers::FoldOut {
        let seq = self.next();
        self.fold(in_line(seq, raw))
    }

    /// Logs a line pi wrote.
    fn read(&mut self, value: Value) -> controlroom_drivers::FoldOut {
        let seq = self.next();
        self.fold(out_line(seq, &value.to_string()))
    }

    /// Encodes and logs an intent's lines.
    fn send(&mut self, intent: &Intent, tag: &LineTag) -> Vec<Value> {
        let lines = self.driver.encode(intent, tag).expect("encodes");
        lines
            .iter()
            .map(|line| {
                self.write(line);
                serde_json::from_str(line).expect("JSON line")
            })
            .collect()
    }

    /// pi's success response to a line.
    fn ok(&mut self, id: &str, command: &str, data: Value) -> Vec<DriverEvent> {
        let mut response =
            json!({"id": id, "type": "response", "command": command, "success": true});
        if !data.is_null() {
            response["data"] = data;
        }
        self.read(response).events
    }

    /// Starts a run with a tagged prompt.
    fn start_run(&mut self, tag: &LineTag) {
        let lines = self.send(&prompt("go"), tag);
        let id = lines[0]["id"].as_str().expect("id").to_owned();
        self.ok(&id, "prompt", json!({"disposition": "started"}));
        self.read(json!({"type": "agent_start"}));
    }
}

fn op(name: &str) -> LineTag {
    LineTag::Op {
        op_id: OpId::new(name),
        part: 0,
    }
}

fn part(tag: &LineTag, part: u16) -> LineTag {
    match tag.clone() {
        LineTag::Op { op_id, .. } => LineTag::Op { op_id, part },
        LineTag::Queue { queue_id, .. } => LineTag::Queue { queue_id, part },
        LineTag::Internal { key, .. } => LineTag::Internal { key, part },
    }
}

fn text(text: &str) -> Vec<ContentPart> {
    vec![ContentPart::text(text)]
}

fn prompt(message: &str) -> Intent {
    Intent::Prompt {
        content: text(message),
    }
}

#[test]
fn prompt_round_trip_starts_a_run_caused_by_its_tag() {
    let mut session = Session::new();
    let tag = op("sched:1700000000000");
    let line = session
        .driver
        .encode(&prompt("Say hello."), &tag)
        .expect("encodes");
    assert_eq!(
        serde_json::from_str::<Value>(&line[0]).expect("json"),
        json!({"id": "cr:op:0:sched:1700000000000", "type": "prompt", "message": "Say hello."})
    );
    let out = session.write(&line[0]);
    assert_eq!(out.events, [DriverEvent::Written { tag: tag.clone() }]);
    assert_eq!(session.driver.view().unanswered, std::slice::from_ref(&tag));

    // pi answers `started` before `agent_start`: the accept waits for the run.
    let events = session.ok(
        "cr:op:0:sched:1700000000000",
        "prompt",
        json!({"disposition": "started"}),
    );
    assert_eq!(events, [DriverEvent::Ready]);
    assert!(!session.driver.view().run_active);
    assert_eq!(session.driver.view().unanswered, std::slice::from_ref(&tag));

    let out = session.read(json!({"type": "agent_start"}));
    assert_eq!(
        out.events,
        [
            DriverEvent::RunStarted {
                cause: Some(tag.clone())
            },
            DriverEvent::Accepted { tag }
        ]
    );
    assert!(session.driver.view().unanswered.is_empty());
    let TimelineOp::Upsert { item } = &out.ops[0] else {
        panic!("no run item");
    };
    assert!(matches!(
        item.body,
        ItemBody::Run {
            trigger: RunTrigger::User { .. },
            ..
        }
    ));
    assert!(session.driver.view().run_active);

    let out = session.read(json!({"type": "agent_settled", "aborted": false}));
    assert_eq!(
        out.events,
        [DriverEvent::RunSettled {
            outcome: RunOutcome::Completed,
            error: None
        }]
    );
}

#[test]
fn queue_tags_trigger_queue_runs_and_rejections_report_the_error() {
    let mut session = Session::new();
    let tag = LineTag::Queue {
        queue_id: QueueId::generate(),
        part: 0,
    };
    let lines = session.send(&prompt("queued"), &tag);
    let id = lines[0]["id"].as_str().expect("id").to_owned();
    session.ok(&id, "prompt", json!({"disposition": "started"}));
    let out = session.read(json!({"type": "agent_start"}));
    let TimelineOp::Upsert { item } = &out.ops[0] else {
        panic!("no run item");
    };
    assert!(matches!(
        item.body,
        ItemBody::Run {
            trigger: RunTrigger::Queue { .. },
            ..
        }
    ));

    // A prompt that pi rejects (e.g. a race with a run the driver hasn't seen yet).
    let busy = op("busy");
    session.write(r#"{"id":"cr:op:0:busy","type":"prompt","message":"x"}"#);
    let out = session.read(json!({
        "id": "cr:op:0:busy", "type": "response", "command": "prompt", "success": false,
        "error": "Agent is already processing."
    }));
    assert_eq!(
        out.events,
        [DriverEvent::Rejected {
            tag: busy,
            error: "Agent is already processing.".into()
        }]
    );
    assert!(matches!(
        &out.ops[0],
        TimelineOp::Upsert { item } if matches!(item.body, ItemBody::Error { .. })
    ));
}

#[test]
fn intents_check_the_run_state() {
    let mut session = Session::new();
    let tag = op("a");
    let steer = Intent::Steer {
        content: text("now"),
    };
    let follow_up = Intent::FollowUp {
        content: text("later"),
    };
    let inject = Intent::Inject {
        origin: InjectOrigin::Job,
        content: text("job done"),
    };
    assert!(matches!(
        session.driver.encode(&steer, &tag),
        Err(EncodeError::InvalidState(_))
    ));
    assert!(matches!(
        session.driver.encode(&follow_up, &tag),
        Err(EncodeError::InvalidState(_))
    ));
    let idle_inject = session.driver.encode(&inject, &tag).expect("inject");
    assert!(
        idle_inject[0].contains(r#""type":"prompt""#),
        "{idle_inject:?}"
    );
    assert!(matches!(
        session.driver.encode(&prompt(""), &tag),
        Err(EncodeError::Invalid(_))
    ));

    session.start_run(&op("run"));
    assert!(matches!(
        session.driver.encode(&prompt("again"), &tag),
        Err(EncodeError::InvalidState(_))
    ));
    assert!(matches!(
        session
            .driver
            .encode(&Intent::NewSession { parent: None }, &tag),
        Err(EncodeError::InvalidState(_))
    ));
    let busy_inject = session.driver.encode(&inject, &tag).expect("inject");
    assert!(
        busy_inject[0].contains(r#""type":"steer""#),
        "{busy_inject:?}"
    );
    let lines = session.driver.encode(&follow_up, &tag).expect("follow-up");
    assert_eq!(
        serde_json::from_str::<Value>(&lines[0]).expect("json"),
        json!({"id": "cr:op:0:a", "type": "follow_up", "message": "later"})
    );
}

/// A steer is shown once pi accepts it, and moves to where pi delivers it.
#[test]
fn accepted_steer_is_shown_and_moves_to_its_delivery() {
    let mut session = Session::new();
    session.start_run(&op("run"));
    let tag = LineTag::Internal {
        key: "send-now:1".into(),
        part: 0,
    };
    let lines = session.send(
        &Intent::Steer {
            content: text("Change of plan."),
        },
        &tag,
    );
    let written = session.seq;
    let out = session.ok(
        lines[0]["id"].as_str().expect("id"),
        "steer",
        json!({"disposition": "queued"}),
    );
    assert_eq!(out, [DriverEvent::Accepted { tag }]);
    session.read(json!({"type": "turn_start"}));
    let out = session.read(json!({"type": "message_start", "message": {
        "role": "user", "content": [{"type": "text", "text": "Change of plan."}]
    }}));
    let [TimelineOp::Upsert { item }] = out.ops.as_slice() else {
        panic!("expected one upsert: {:?}", out.ops);
    };
    assert_eq!(item.id.0, format!("user-{written}"));
    assert_eq!(item.seq_from.0, session.seq);
}

/// Cancel answers the open dialogs first, then aborts; the fold accepts the dialog answers
/// at once (pi never responds to them) and the abort when pi answers it.
#[test]
fn cancel_with_an_open_dialog_cancels_it_before_aborting() {
    let mut session = Session::new();
    session.start_run(&op("run"));
    let out = session.read(json!({
        "type": "extension_ui_request", "id": "dlg-1", "method": "select",
        "title": "Allow: rm -rf build?", "options": ["Allow", "Block"]
    }));
    let [DriverEvent::RequestOpened(request)] = out.events.as_slice() else {
        panic!("no request: {:?}", out.events);
    };
    assert_eq!(request.kind, RequestKind::Dialog);
    assert!(request.blocking);
    assert_eq!(
        session.driver.view().open_requests,
        std::slice::from_ref(&request.id)
    );

    let tag = op("cancel");
    let lines = session
        .driver
        .encode(&Intent::Cancel, &tag)
        .expect("cancel");
    let parsed: Vec<Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).expect("json"))
        .collect();
    assert_eq!(
        parsed,
        [
            json!({"type": "extension_ui_response", "id": "dlg-1", "cancelled": true, "crTag": "cr:op:0:cancel"}),
            json!({"type": "abort", "id": "cr:op:1:cancel"}),
        ]
    );

    let out = session.write(&lines[0]);
    assert_eq!(
        out.events,
        [
            DriverEvent::Written { tag: part(&tag, 0) },
            DriverEvent::Accepted { tag: part(&tag, 0) }
        ]
    );
    let TimelineOp::Upsert { item } = &out.ops[0] else {
        panic!("no request update");
    };
    let ItemBody::Request { request: answered } = &item.body else {
        panic!("not a request");
    };
    assert_eq!(answered.state, RequestState::Answered);
    assert_eq!(
        answered.answer.as_ref().map(|a| &a.response),
        Some(&Response::Dismiss)
    );
    assert!(session.driver.view().open_requests.is_empty());

    session.write(&lines[1]);
    assert_eq!(session.driver.view().unanswered, [part(&tag, 1)]);
    session.read(json!({"type": "agent_settled", "aborted": true}));
    let events = session.ok("cr:op:1:cancel", "abort", Value::Null);
    assert_eq!(events, [DriverEvent::Accepted { tag: part(&tag, 1) }]);
    assert_eq!(
        session.driver.view().last_outcome,
        Some(RunOutcome::Cancelled)
    );
}

#[test]
fn answers_map_to_dialog_responses() {
    let mut session = Session::new();
    session.read(json!({
        "type": "extension_ui_request", "id": "dlg-2", "method": "input",
        "title": "Your name", "placeholder": "type a name"
    }));
    let request = session.driver.view().open_requests[0].clone();
    let tag = op("answer");
    let wrong = Intent::Answer {
        request: request.clone(),
        response: Response::Choice {
            option_id: "0".into(),
            reason: None,
        },
    };
    assert!(matches!(
        session.driver.encode(&wrong, &tag),
        Err(EncodeError::Invalid(_))
    ));
    let answer = Intent::Answer {
        request: request.clone(),
        response: Response::Answers {
            answers: vec![QuestionAnswer {
                question_id: "input".into(),
                selected: vec![],
                text: Some("alice".into()),
            }],
        },
    };
    let lines = session.send(&answer, &tag);
    assert_eq!(
        lines,
        [
            json!({"type": "extension_ui_response", "id": "dlg-2", "value": "alice", "crTag": "cr:op:0:answer"})
        ]
    );
    assert!(session.driver.view().open_requests.is_empty());
    // The request is closed now: answering again is refused.
    assert!(matches!(
        session.driver.encode(&answer, &tag),
        Err(EncodeError::Invalid(_))
    ));
}

#[test]
fn a_late_answer_to_an_expired_dialog_is_rejected() {
    let mut session = Session::new();
    session.read(json!({
        "type": "extension_ui_request", "id": "dlg-3", "method": "confirm",
        "title": "Quick?", "timeout": 5
    }));
    let opened_at = session.seq;
    let tag = op("late");
    let answer = Intent::Answer {
        request: session.driver.view().open_requests[0].clone(),
        response: Response::Choice {
            option_id: "yes".into(),
            reason: None,
        },
    };
    let line = session.driver.encode(&answer, &tag).expect("encodes")[0].clone();
    // Logged 10 ms after the dialog: past its 5 ms timeout.
    let out = session.fold(Record {
        seq: opened_at + 1,
        ts: opened_at * 10 + 10,
        body: Body::In(controlroom_transcript::Line::new(line)),
    });
    assert!(matches!(
        out.events.as_slice(),
        [
            DriverEvent::Written { tag: w },
            DriverEvent::RequestClosed { .. },
            DriverEvent::Rejected { tag: t, .. },
        ] if *t == tag && *w == tag
    ));
}

#[test]
fn refresh_reports_the_session_and_options_and_set_option_round_trips() {
    let mut session = Session::new();
    let tag = LineTag::Internal {
        key: "refresh".into(),
        part: 0,
    };
    let lines = session.send(&Intent::Refresh, &tag);
    let kinds: Vec<&str> = lines
        .iter()
        .map(|l| l["type"].as_str().expect("type"))
        .collect();
    assert_eq!(
        kinds,
        [
            "get_state",
            "get_available_models",
            "get_available_thinking_levels"
        ]
    );
    let model = |id: &str| json!({"id": id, "name": id.to_uppercase(), "provider": "mock", "contextWindow": 1000});
    let events = session.ok(
        "cr:in:0:refresh",
        "get_state",
        json!({"model": model("m1"), "thinkingLevel": "low", "sessionId": "s1",
               "sessionFile": "/home/alice/.pi/agent/sessions/s1.jsonl", "sessionName": "demo"}),
    );
    assert!(events.contains(&DriverEvent::NativeSession(NativeRef {
        id: "s1".into(),
        path: Some("/home/alice/.pi/agent/sessions/s1.jsonl".into()),
    })));
    assert!(events.contains(&DriverEvent::Title("demo".into())));
    session.ok(
        "cr:in:1:refresh",
        "get_available_models",
        json!({"models": [model("m1"), model("m2")]}),
    );
    session.ok(
        "cr:in:2:refresh",
        "get_available_thinking_levels",
        json!({"levels": ["off", "low", "high"]}),
    );
    let options = &session.driver.view().options;
    assert_eq!(options.len(), 2);

    let set = Intent::SetOption {
        id: "model".into(),
        value: OptionValue::Choice("mock/m2".into()),
    };
    let tag = op("set");
    let lines = session.send(&set, &tag);
    assert_eq!(
        lines,
        [json!({"id": "cr:op:0:set", "type": "set_model", "provider": "mock", "modelId": "m2"})]
    );
    let events = session.ok("cr:op:0:set", "set_model", model("m2"));
    assert_eq!(events[0], DriverEvent::Accepted { tag });
    assert!(matches!(&events[1], DriverEvent::Options(o) if o[0].control
    == controlroom_protocol::session::OptionControl::Select {
        value: "mock/m2".into(),
        choices: vec![
            controlroom_protocol::session::OptionChoice { value: "mock/m1".into(), label: "M1".into() },
            controlroom_protocol::session::OptionChoice { value: "mock/m2".into(), label: "M2".into() },
        ],
    }));
    let bad = Intent::SetOption {
        id: "thinking".into(),
        value: OptionValue::Choice("max".into()),
    };
    assert!(matches!(
        session.driver.encode(&bad, &op("x")),
        Err(EncodeError::Invalid(_))
    ));
}

#[test]
fn new_session_links_the_parent_and_forgets_the_native_session() {
    let mut session = Session::new();
    session.ok(
        "x",
        "get_state",
        json!({"sessionId": "s1", "sessionFile": "/home/alice/s1.jsonl"}),
    );
    assert!(session.driver.view().native.is_some());
    let tag = op("fresh");
    let lines = session.send(
        &Intent::NewSession {
            parent: Some("/home/alice/s1.jsonl".into()),
        },
        &tag,
    );
    assert_eq!(
        lines,
        [
            json!({"id": "cr:op:0:fresh", "type": "new_session", "parentSession": "/home/alice/s1.jsonl"})
        ]
    );
    session.ok("cr:op:0:fresh", "new_session", json!({"cancelled": false}));
    assert_eq!(session.driver.view().native, None);
    // The runtime passes the native id; pi links the parent by its file.
    session.ok(
        "x",
        "get_state",
        json!({"sessionId": "s2", "sessionFile": "/home/alice/s2.jsonl"}),
    );
    let lines = session
        .driver
        .encode(
            &Intent::NewSession {
                parent: Some("s2".into()),
            },
            &op("again"),
        )
        .expect("encodes");
    assert!(
        lines[0].contains(r#""parentSession":"/home/alice/s2.jsonl""#),
        "{lines:?}"
    );
}

#[test]
fn exit_mid_run_interrupts_and_drops_unanswered_lines() {
    let mut session = Session::new();
    session.start_run(&op("run"));
    session.send(&Intent::Cancel, &op("cancel"));
    assert_eq!(session.driver.view().unanswered.len(), 1);
    let seq = session.next();
    let out = session.fold(Record {
        seq,
        ts: seq * 10,
        body: Body::Exit(controlroom_transcript::ExitInfo {
            code: None,
            signal: Some(9),
            cause: None,
        }),
    });
    assert_eq!(
        out.events,
        [DriverEvent::Exited {
            code: None,
            signal: Some(9),
            during_run: true
        }]
    );
    let view = session.driver.view();
    assert!(!view.run_active && !view.ready && view.unanswered.is_empty());
    assert_eq!(view.last_outcome, Some(RunOutcome::Failed));
}

/// A killed supervisor leaves no `exit` record: the next `meta` ends what the old process
/// had open without an `Exited` (the runtime detects the dead supervisor itself), and the new
/// process reports its native session again even when it resumed the same one.
#[test]
fn meta_resets_the_process_and_the_native_session_is_reported_again() {
    let state = json!({"sessionId": "s1", "sessionFile": "/home/alice/s1.jsonl"});
    let native = DriverEvent::NativeSession(NativeRef {
        id: "s1".into(),
        path: Some("/home/alice/s1.jsonl".into()),
    });
    let mut session = Session::new();
    assert!(
        session
            .ok("x", "get_state", state.clone())
            .contains(&native)
    );
    assert!(session.ok("y", "get_state", state.clone()).is_empty());
    session.start_run(&op("run"));
    session.read(json!({
        "type": "extension_ui_request", "id": "dlg-9", "method": "confirm", "title": "Sure?"
    }));
    let seq = session.next();
    let out = session.fold(Record {
        seq,
        ts: seq * 10,
        body: Body::Meta(Box::default()),
    });
    assert!(
        out.events
            .iter()
            .all(|e| matches!(e, DriverEvent::RequestClosed { .. })),
        "{:?}",
        out.events
    );
    assert!(out.ops.iter().any(|op| matches!(op,
        TimelineOp::Upsert { item } if matches!(item.body,
            ItemBody::Run { outcome: Some(RunOutcome::Failed), .. }))));
    let view = session.driver.view();
    assert!(!view.ready && !view.run_active && view.open_requests.is_empty());
    assert_eq!(view.native, None);
    let events = session.ok("z", "get_state", state);
    assert_eq!(events, [DriverEvent::Ready, native]);
}

#[test]
fn launch_args_start_or_resume_a_native_session() {
    let driver = PiDriver::new();
    assert_eq!(
        driver.launch_args(&StartMode::New {
            native_id: "s1".into()
        }),
        ["--mode", "rpc", "--session-id", "s1"]
    );
    let file = "/home/alice/.pi/agent/sessions/--home-alice-project--/s1.jsonl";
    assert_eq!(
        driver.launch_args(&StartMode::Resume {
            native: NativeRef {
                id: "s1".into(),
                path: Some(file.into()),
            }
        }),
        ["--mode", "rpc", "--session", file]
    );
    assert_eq!(
        driver.launch_args(&StartMode::Resume {
            native: NativeRef {
                id: "s1".into(),
                path: None,
            }
        }),
        ["--mode", "rpc", "--session-id", "s1"]
    );
}

#[test]
fn noise_and_unknown_events_are_ignored() {
    let mut session = Session::new();
    for raw in [
        "Welcome to bash",
        "{\"no\":\"type\"}",
        "[1,2]",
        "{\"type\":\"future_event\"}",
    ] {
        let seq = session.next();
        let out = session.fold(out_line(seq, raw));
        assert!(out.ops.is_empty(), "{raw}");
        assert!(out.events.iter().all(|e| *e == DriverEvent::Ready), "{raw}");
    }
}
