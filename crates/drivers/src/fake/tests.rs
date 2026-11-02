//! The fake driver and agent together: encode, run the agent, fold the transcript.

use controlroom_protocol::ids::{OpId, QueueId};
use controlroom_protocol::session::{ContentPart, RequestKind, Response, RunOutcome};
use controlroom_protocol::timeline::{ItemBody, TimelineOp};
use controlroom_transcript::{Body, ExitInfo, Line, Record};

use super::{FakeAgent, FakeDriver, FakeScript, FakeStep};
use crate::{Capabilities, Driver, DriverEvent, FoldOut, Intent, LineTag, StartMode};

/// A transcript written by hand: lines in, the agent's lines out.
struct Rig {
    agent: FakeAgent,
    records: Vec<Record>,
}

impl Rig {
    fn new(script: FakeScript) -> Self {
        let driver = FakeDriver::default();
        let args = driver.launch_args(&StartMode::New {
            native_id: "n1".into(),
        });
        let mut rig = Rig {
            agent: FakeAgent::launch(script, &args),
            records: Vec::new(),
        };
        rig.push(Body::Meta(Box::default()));
        rig
    }

    fn push(&mut self, body: Body) {
        let seq = self.records.len() as u64 + 1;
        self.records.push(Record {
            seq,
            ts: seq * 10,
            body,
        });
    }

    fn write(&mut self, driver: &mut FakeDriver, intent: &Intent, tag: &LineTag) {
        for line in driver.encode(intent, tag).unwrap() {
            let out = self.agent.input(&line);
            self.push(Body::In(Line::new(line)));
            self.output(out);
        }
    }

    fn output(&mut self, out: super::AgentOutput) {
        for line in out.lines {
            self.push(Body::Out(Line::new(line)));
        }
        if let Some(code) = out.exit {
            self.push(Body::Exit(ExitInfo {
                code: Some(code),
                signal: None,
                cause: None,
            }));
        }
    }

    /// Folds records the driver hasn't seen.
    fn fold(&self, driver: &mut FakeDriver) -> Vec<FoldOut> {
        let from = driver.view().last_seq.map_or(0, |s| s.0);
        self.records
            .iter()
            .filter(|r| r.seq > from)
            .map(|r| driver.fold(r))
            .collect()
    }
}

fn events(outs: &[FoldOut]) -> Vec<DriverEvent> {
    outs.iter().flat_map(|o| o.events.clone()).collect()
}

fn prompt(text: &str) -> Intent {
    Intent::Prompt {
        content: vec![ContentPart::text(text)],
    }
}

#[test]
fn a_prompt_runs_and_refolds_identically() {
    let mut rig = Rig::new(
        FakeScript::default()
            .default_turn([FakeStep::Text("hel".into()), FakeStep::Text("lo".into())]),
    );
    let mut driver = FakeDriver::new(Capabilities::default());
    rig.fold(&mut driver);
    let refresh = LineTag::Internal {
        key: "refresh".into(),
        part: 0,
    };
    rig.write(&mut driver, &Intent::Refresh, &refresh);
    let tag = LineTag::Queue {
        queue_id: QueueId::generate(),
        part: 0,
    };
    rig.write(&mut driver, &prompt("hi"), &tag);
    let outs = rig.fold(&mut driver);
    let events = events(&outs);
    assert!(events.contains(&DriverEvent::Ready));
    assert!(events.contains(&DriverEvent::Written { tag: tag.clone() }));
    assert!(events.contains(&DriverEvent::RunStarted {
        cause: Some(tag.clone())
    }));
    assert!(events.contains(&DriverEvent::Accepted { tag }));
    assert!(events.contains(&DriverEvent::RunSettled {
        outcome: RunOutcome::Completed,
        error: None
    }));
    let view = driver.view();
    assert!(!view.run_active && view.unanswered.is_empty());
    assert_eq!(view.native.as_ref().unwrap().id, "n1");
    let appended: String = outs
        .iter()
        .flat_map(|o| &o.ops)
        .filter_map(|op| match op {
            TimelineOp::AppendText { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(appended, "lo");

    // A re-fold from scratch gives the same ops and events.
    let mut again = FakeDriver::new(Capabilities::default());
    let all = rig.fold(&mut again);
    let mut first = FakeDriver::new(Capabilities::default());
    assert_eq!(rig.fold(&mut first), all);
}

#[test]
fn requests_wait_for_their_answer_and_gates_for_release() {
    let mut rig = Rig::new(FakeScript::default().default_turn([
        FakeStep::Request {
            kind: RequestKind::Permission,
            title: "deploy".into(),
        },
        FakeStep::Tool {
            title: "deploy".into(),
            output: "ok".into(),
            gate: Some("g".into()),
        },
    ]));
    let mut driver = FakeDriver::new(Capabilities::default());
    rig.write(
        &mut driver,
        &prompt("go"),
        &LineTag::Op {
            op_id: OpId::new("p"),
            part: 0,
        },
    );
    let request = events(&rig.fold(&mut driver))
        .into_iter()
        .find_map(|e| match e {
            DriverEvent::RequestOpened(r) => Some(r.id),
            _ => None,
        })
        .unwrap();
    assert_eq!(driver.view().open_requests, std::slice::from_ref(&request));
    let answer = Intent::Answer {
        request,
        response: Response::Choice {
            option_id: "allow-once".into(),
            reason: None,
        },
    };
    rig.write(
        &mut driver,
        &answer,
        &LineTag::Op {
            op_id: OpId::new("a"),
            part: 0,
        },
    );
    rig.fold(&mut driver);
    assert!(driver.view().open_requests.is_empty());
    assert_eq!(driver.view().open_tools, 1, "held at the gate");
    let out = rig.agent.release("g");
    rig.output(out);
    let outs = rig.fold(&mut driver);
    assert_eq!(driver.view().open_tools, 0);
    assert!(outs.iter().flat_map(|o| &o.ops).any(|op| matches!(
        op,
        TimelineOp::Upsert { item } if matches!(item.body, ItemBody::ToolCall { .. })
    )));
    assert!(!driver.view().run_active);
}

#[test]
fn an_exit_mid_run_and_a_new_process() {
    let mut rig = Rig::new(FakeScript::default().default_turn([FakeStep::Exit(3)]));
    let mut driver = FakeDriver::new(Capabilities::default());
    rig.write(
        &mut driver,
        &prompt("go"),
        &LineTag::Op {
            op_id: OpId::new("p"),
            part: 0,
        },
    );
    let events = events(&rig.fold(&mut driver));
    assert!(events.contains(&DriverEvent::Exited {
        code: Some(3),
        signal: None,
        during_run: true
    }));
    assert!(!driver.view().ready && !driver.view().run_active);
    rig.push(Body::Meta(Box::default()));
    rig.fold(&mut driver);
    assert_eq!(
        driver.view().native,
        None,
        "a new process reports its session again"
    );
}

#[test]
fn unsupported_intents_are_refused() {
    let mut driver = FakeDriver::new(Capabilities::default());
    let tag = LineTag::Op {
        op_id: OpId::new("x"),
        part: 0,
    };
    let steer = Intent::Steer {
        content: vec![ContentPart::text("x")],
    };
    assert!(driver.encode(&steer, &tag).is_err());
    let new_session = Intent::NewSession { parent: None };
    assert!(driver.encode(&new_session, &tag).is_err());
}
