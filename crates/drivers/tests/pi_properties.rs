//! Properties of the PiDriver fold over every fixture: determinism (from scratch and from a
//! mid-point snapshot), robustness against cut and missing records, and exactly one answer
//! per tagged line.

mod support;

use controlroom_drivers::{Driver, DriverEvent, Intent, LineTag, PiDriver};
use controlroom_protocol::daemon::OptionValue;
use controlroom_protocol::ids::{OpId, RequestId};
use controlroom_protocol::session::{ContentPart, Response};
use controlroom_transcript::{Body, Line, Record};
use serde_json::Value;
use support::{fixtures, fold_all};

#[test]
fn refolding_from_scratch_is_identical() {
    for (name, records) in fixtures() {
        let (first, first_outs) = fold_all(&records);
        let (second, second_outs) = fold_all(&records);
        assert_eq!(first_outs, second_outs, "{name}");
        assert_eq!(first.view(), second.view(), "{name}");
    }
}

/// A snapshot (a clone of the driver) taken after any record continues exactly like the
/// original.
#[test]
fn folding_on_from_any_snapshot_is_identical() {
    for (name, records) in fixtures() {
        let (_, expected) = fold_all(&records);
        let mut driver = PiDriver::new();
        for split in 0..records.len() {
            let mut snapshot = driver.clone();
            let rest: Vec<_> = records[split..].iter().map(|r| snapshot.fold(r)).collect();
            assert_eq!(
                rest,
                expected[split..],
                "{name}: snapshot after {split} records"
            );
            driver.fold(&records[split]);
        }
    }
}

/// Every intent, to exercise `encode` in whatever state the driver is in.
fn intents(driver: &PiDriver) -> Vec<Intent> {
    let request = driver
        .view()
        .open_requests
        .first()
        .cloned()
        .unwrap_or_else(|| RequestId::new("dialog-1"));
    let text = vec![ContentPart::text("hi")];
    vec![
        Intent::Prompt {
            content: text.clone(),
        },
        Intent::Steer {
            content: text.clone(),
        },
        Intent::FollowUp {
            content: text.clone(),
        },
        Intent::Inject {
            origin: controlroom_protocol::timeline::InjectOrigin::Job,
            content: text,
        },
        Intent::Cancel,
        Intent::Answer {
            request: request.clone(),
            response: Response::Dismiss,
        },
        Intent::Answer {
            request,
            response: Response::Choice {
                option_id: "0".into(),
                reason: None,
            },
        },
        Intent::SetOption {
            id: "model".into(),
            value: OptionValue::Choice("mock/mock-model-b".into()),
        },
        Intent::SetOption {
            id: "thinking".into(),
            value: OptionValue::Bool(true),
        },
        Intent::NewSession { parent: None },
        Intent::Refresh,
    ]
}

/// Cutting any one line (marked truncated, or cut silently so it no longer parses) or
/// dropping it never panics, and every prefix of every fixture encodes every intent without
/// panicking. Encoded lines are single JSON lines.
#[test]
fn cut_and_missing_records_never_panic() {
    let tag = LineTag::Op {
        op_id: OpId::new("op"),
        part: 0,
    };
    for (name, records) in fixtures() {
        let mut prefix = PiDriver::new();
        for (index, record) in records.iter().enumerate() {
            for intent in intents(&prefix) {
                for line in prefix.encode(&intent, &tag).unwrap_or_default() {
                    assert!(!line.contains('\n'), "{name}: {line}");
                    serde_json::from_str::<Value>(&line).expect("encoded JSON");
                }
            }
            prefix.fold(record);
            let Some(line) = record.line() else {
                continue;
            };
            let half = line.raw.char_indices().nth(line.raw.chars().count() / 2);
            let cut = half.map_or(0, |(at, _)| at);
            let variants = [
                Some(Line {
                    raw: line.raw[..cut].to_owned(),
                    truncated: (line.raw.len() - cut) as u64 + 1,
                    lossy: false,
                }),
                Some(Line::new(&line.raw[..cut])),
                None,
            ];
            for variant in variants {
                let mut driver = PiDriver::new();
                for (other, original) in records.iter().enumerate() {
                    if other != index {
                        driver.fold(original);
                        continue;
                    }
                    let Some(variant) = &variant else {
                        continue;
                    };
                    let body = match &original.body {
                        Body::In(_) => Body::In(variant.clone()),
                        Body::Out(_) => Body::Out(variant.clone()),
                        _ => Body::Stderr(variant.clone()),
                    };
                    driver.fold(&Record {
                        seq: original.seq,
                        ts: original.ts,
                        body,
                    });
                }
                let view = driver.view();
                assert!(!view.run_active && view.open_requests.is_empty(), "{name}");
            }
        }
    }
}

/// Rewrites a fixture as if the runtime had written its lines: every `in` line gets a tag in
/// its `id` (or `crTag` for dialog answers), and pi's responses echo it.
fn tagged(records: &[Record]) -> Vec<Record> {
    let tag_for = |id: &str| format!("cr:op:0:fixture-{id}");
    records
        .iter()
        .map(|record| {
            let body = match &record.body {
                Body::In(line) | Body::Out(line) => {
                    let Ok(Value::Object(mut map)) = serde_json::from_str::<Value>(&line.raw)
                    else {
                        return record.clone();
                    };
                    let is_in = matches!(record.body, Body::In(_));
                    let kind = map.get("type").and_then(Value::as_str).unwrap_or("");
                    let id = map.get("id").and_then(Value::as_str).map(str::to_owned);
                    match (is_in, kind, id) {
                        (true, "extension_ui_response", Some(id)) => {
                            map.insert("crTag".into(), tag_for(&id).into());
                        }
                        (true, _, Some(id)) | (false, "response", Some(id)) => {
                            map.insert("id".into(), tag_for(&id).into());
                        }
                        _ => return record.clone(),
                    }
                    let line = Line::new(Value::Object(map).to_string());
                    if is_in {
                        Body::In(line)
                    } else {
                        Body::Out(line)
                    }
                }
                other => other.clone(),
            };
            Record {
                seq: record.seq,
                ts: record.ts,
                body,
            }
        })
        .collect()
}

/// Every tagged line is answered at most once; every response and every dialog answer that
/// reached an open dialog is reported; nothing stays unanswered once pi exits.
#[test]
fn tagged_lines_are_answered_exactly_once() {
    for (name, records) in fixtures() {
        let records = tagged(&records);
        let mut driver = PiDriver::new();
        let mut answered: Vec<LineTag> = Vec::new();
        let mut written = 0;
        for record in &records {
            if matches!(record.body, Body::In(_))
                && record.raw().is_some_and(|r| r.contains("cr:op"))
            {
                written += 1;
            }
            for event in driver.fold(record).events {
                match event {
                    DriverEvent::Accepted { tag } | DriverEvent::Rejected { tag, .. } => {
                        assert!(!answered.contains(&tag), "{name}: {tag:?} answered twice");
                        answered.push(tag);
                    }
                    DriverEvent::RequestClosed { id } => {
                        // Only dialogs we never answered close this way.
                        assert!(name.contains("timeout"), "{name}: {id} closed unanswered");
                    }
                    _ => {}
                }
            }
        }
        let unanswered_at_exit = written - answered.len();
        // Lines pi never answered: the ones in flight when it died, and a late dialog answer
        // (rejected, so counted) — so only crash-like fixtures may leave some.
        let crashed = ["sighup", "sigterm", "crash", "stdin-close"];
        if !crashed.iter().any(|c| name.starts_with(c)) {
            assert_eq!(unanswered_at_exit, 0, "{name}");
        }
        assert!(driver.view().unanswered.is_empty(), "{name}");
    }
}
