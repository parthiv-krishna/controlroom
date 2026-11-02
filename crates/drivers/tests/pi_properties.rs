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
use support::{fixtures, fold_all, tagged};

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

/// The `id` and `disposition` of every successful `prompt` response, by id.
fn prompt_dispositions(records: &[Record]) -> Vec<(String, String)> {
    records
        .iter()
        .filter(|r| matches!(r.body, Body::Out(_)))
        .filter_map(|r| serde_json::from_str::<Value>(r.raw()?).ok())
        .filter(|v| v["type"] == "response" && v["command"] == "prompt" && v["success"] == true)
        .filter_map(|v| {
            let id = v["id"].as_str()?.to_owned();
            Some((id, v["data"]["disposition"].as_str()?.to_owned()))
        })
        .collect()
}

/// For the runtime written as if it had written every line (see `tagged`):
/// - every tagged `in` line is reported `Written`, before the other events of its record;
/// - a tag is `Accepted`/`Rejected` only after its `Written`, and at most once;
/// - a prompt that starts a run is `Accepted` only once the run is active, after
///   `RunStarted` (pi answers `started` before `agent_start`);
/// - every response and every answer to an open dialog is reported; nothing stays
///   unanswered once pi exits.
#[test]
fn tagged_lines_are_written_then_answered_once() {
    let mut run_starting_prompts = 0;
    for (name, records) in fixtures() {
        let records = tagged(&records);
        let starting: Vec<LineTag> = prompt_dispositions(&records)
            .into_iter()
            .filter(|(_, disposition)| disposition == "started")
            .map(|(id, _)| LineTag::Op {
                op_id: OpId::new(id.trim_start_matches("cr:op:0:")),
                part: 0,
            })
            .collect();
        run_starting_prompts += starting.len();
        let mut driver = PiDriver::new();
        let mut written: Vec<LineTag> = Vec::new();
        let mut answered: Vec<LineTag> = Vec::new();
        let mut tagged_lines = 0;
        for record in &records {
            let is_tagged_in = matches!(record.body, Body::In(_))
                && record.raw().is_some_and(|r| r.contains("cr:op"));
            if is_tagged_in {
                tagged_lines += 1;
            }
            let events = driver.fold(record).events;
            if is_tagged_in {
                assert!(
                    matches!(events.first(), Some(DriverEvent::Written { .. })),
                    "{name}: seq {} does not start with Written: {events:?}",
                    record.seq
                );
            }
            let mut run_started = false;
            for event in events {
                match event {
                    DriverEvent::Written { tag } => {
                        assert!(!written.contains(&tag), "{name}: {tag:?} written twice");
                        written.push(tag);
                    }
                    DriverEvent::RunStarted { .. } => run_started = true,
                    DriverEvent::Accepted { tag } | DriverEvent::Rejected { tag, .. } => {
                        assert!(written.contains(&tag), "{name}: {tag:?} answered unwritten");
                        assert!(!answered.contains(&tag), "{name}: {tag:?} answered twice");
                        if starting.contains(&tag) {
                            assert!(
                                run_started && driver.view().run_active,
                                "{name}: run-starting {tag:?} accepted while idle"
                            );
                        }
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
        assert_eq!(written.len(), tagged_lines, "{name}");
        // Lines pi never answered: the ones in flight when it died, and a late dialog answer
        // (rejected, so counted), so only crash-like fixtures may leave some.
        let crashed = ["sighup", "sigterm", "crash", "stdin-close"];
        if !crashed.iter().any(|c| name.starts_with(c)) {
            assert_eq!(answered.len(), tagged_lines, "{name}");
        }
        assert!(driver.view().unanswered.is_empty(), "{name}");
    }
    assert!(
        run_starting_prompts > 20,
        "too few run-starting prompts checked"
    );
}
