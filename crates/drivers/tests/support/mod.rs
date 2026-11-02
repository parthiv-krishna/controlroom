//! Shared helpers for the PiDriver tests: fixture loading, folding, a reference timeline
//! materializer, and JSON renderings of events and views for goldens.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::collections::BTreeMap;
use std::path::PathBuf;

use controlroom_drivers::{AgentView, Driver, DriverEvent, FoldOut, LineTag, PiDriver};
use controlroom_protocol::session::ContentPart;
use controlroom_protocol::timeline::{ItemBody, TimelineItem, TimelineOp, ToolOutput};
use controlroom_transcript::{Body, Line, Record};
use serde_json::{Value, json};

/// `fixtures/pi-rpc/1.2.0`.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pi-rpc/1.2.0")
}

/// Every fixture transcript as `(name, records)`, sorted by name.
pub fn fixtures() -> Vec<(String, Vec<Record>)> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir())
        .expect("fixtures dir")
        .filter_map(|entry| {
            let name = entry.expect("dir entry").file_name().into_string().ok()?;
            name.strip_suffix(".jsonl").map(str::to_owned)
        })
        .collect();
    names.sort();
    assert!(
        names.len() >= 30,
        "expected the pi fixtures, found {names:?}"
    );
    names
        .into_iter()
        .map(|name| {
            let records = load(&fixtures_dir().join(format!("{name}.jsonl")));
            (name, records)
        })
        .collect()
}

/// Parses a transcript file.
pub fn load(path: &std::path::Path) -> Vec<Record> {
    let text = std::fs::read_to_string(path).expect("read fixture");
    text.lines()
        .map(|line| serde_json::from_str(line).expect("transcript record"))
        .collect()
}

/// Folds every record into a fresh driver.
pub fn fold_all(records: &[Record]) -> (PiDriver, Vec<FoldOut>) {
    let mut driver = PiDriver::new();
    let outs = records.iter().map(|r| driver.fold(r)).collect();
    (driver, outs)
}

/// A line record.
pub fn record(seq: u64, ts: u64, body: Body) -> Record {
    Record { seq, ts, body }
}

/// An `in` record.
pub fn in_line(seq: u64, raw: &str) -> Record {
    record(seq, seq * 10, Body::In(Line::new(raw)))
}

/// An `out` record.
pub fn out_line(seq: u64, raw: &str) -> Record {
    record(seq, seq * 10, Body::Out(Line::new(raw)))
}

/// Applies ops the way the protocol defines them, checking that every op targets an item
/// that exists (and `end` one that is streaming).
#[derive(Debug, Default)]
pub struct Timeline {
    pub items: BTreeMap<String, TimelineItem>,
}

impl Timeline {
    pub fn apply(&mut self, op: &TimelineOp) {
        match op {
            TimelineOp::Upsert { item } => {
                assert!(item.seq_from <= item.seq_to, "bad range in {item:?}");
                self.items.insert(item.id.0.clone(), (**item).clone());
            }
            TimelineOp::AppendText {
                id,
                text,
                seq_from,
                seq_to,
            } => {
                let item = self.items.get_mut(&id.0).expect("append to a known item");
                assert!(item.streaming, "append to a finished item {id}");
                assert!(*seq_from >= item.seq_from && seq_from <= seq_to);
                append(&mut item.body, text);
                item.seq_to = *seq_to;
            }
            TimelineOp::End { id, seq_to, .. } => {
                let item = self.items.get_mut(&id.0).expect("end a known item");
                assert!(item.streaming, "end of a finished item {id}");
                item.streaming = false;
                item.seq_to = *seq_to;
            }
            TimelineOp::Unknown => panic!("unknown op"),
        }
    }

    /// The items ordered as a client shows them.
    pub fn ordered(&self) -> Vec<&TimelineItem> {
        let mut items: Vec<&TimelineItem> = self.items.values().collect();
        items.sort_by(|a, b| (a.seq_from, &a.id).cmp(&(b.seq_from, &b.id)));
        items
    }
}

fn append(body: &mut ItemBody, text: &str) {
    match body {
        ItemBody::Message { content, .. } => match content.last_mut() {
            Some(ContentPart::Text { text: last }) => last.push_str(text),
            _ => content.push(ContentPart::text(text)),
        },
        ItemBody::Thinking { text: thinking } => thinking.push_str(text),
        ItemBody::ToolCall { output, .. } => match output {
            Some(ToolOutput::Text { text: out } | ToolOutput::Terminal { text: out }) => {
                out.push_str(text)
            }
            other => *other = Some(ToolOutput::Text { text: text.into() }),
        },
        other => panic!("append to {other:?}"),
    }
}

/// A tag as JSON.
pub fn tag_json(tag: &LineTag) -> Value {
    match tag {
        LineTag::Op { op_id, part } => json!({"op": op_id, "part": part}),
        LineTag::Queue { queue_id, part } => json!({"queue": queue_id, "part": part}),
        LineTag::Internal { key, part } => json!({"internal": key, "part": part}),
    }
}

/// An event as JSON.
pub fn event_json(event: &DriverEvent) -> Value {
    match event {
        DriverEvent::Ready => json!({"event": "ready"}),
        DriverEvent::NativeSession(native) => {
            json!({"event": "native_session", "id": native.id, "path": native.path})
        }
        DriverEvent::RunStarted { cause } => {
            json!({"event": "run_started", "cause": cause.as_ref().map(tag_json)})
        }
        DriverEvent::RunSettled { outcome, error } => {
            json!({"event": "run_settled", "outcome": outcome, "error": error})
        }
        DriverEvent::Accepted { tag } => json!({"event": "accepted", "tag": tag_json(tag)}),
        DriverEvent::Rejected { tag, error } => {
            json!({"event": "rejected", "tag": tag_json(tag), "error": error})
        }
        DriverEvent::RequestOpened(request) => {
            json!({"event": "request_opened", "request": request})
        }
        DriverEvent::RequestClosed { id } => json!({"event": "request_closed", "id": id}),
        DriverEvent::Context(context) => json!({"event": "context", "context": context}),
        DriverEvent::Options(options) => json!({"event": "options", "options": options}),
        DriverEvent::Title(title) => json!({"event": "title", "title": title}),
        DriverEvent::Exited {
            code,
            signal,
            during_run,
        } => json!({"event": "exited", "code": code, "signal": signal, "during_run": during_run}),
    }
}

/// A view as JSON.
pub fn view_json(view: &AgentView) -> Value {
    json!({
        "ready": view.ready,
        "run_active": view.run_active,
        "open_tools": view.open_tools,
        "open_requests": view.open_requests,
        "last_outcome": view.last_outcome,
        "native": view.native.as_ref().map(|n| json!({"id": n.id, "path": n.path})),
        "options": view.options,
        "unanswered": view.unanswered.iter().map(tag_json).collect::<Vec<_>>(),
        "last_seq": view.last_seq,
        "last_ts": view.last_ts,
    })
}
