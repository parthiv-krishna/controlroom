//! Golden fold tests: every pi fixture, with its lines tagged as the runtime would write them,
//! folded into timeline ops and driver events, plus the materialized timeline and the final
//! view, compared with `tests/golden/pi/<fixture>.json`.
//!
//! Regenerate with `UPDATE_GOLDEN=1 cargo test -p controlroom-drivers --test pi_golden`, then
//! review the diff by eye.

mod support;

use std::path::PathBuf;

use controlroom_drivers::Driver;
use serde_json::{Value, json};
use support::{Timeline, event_json, fixtures, fold_all, tagged, view_json};

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/pi")
}

/// Renders a fixture fold as JSON with one op, event or item per line, so diffs stay readable.
fn render(records: &[controlroom_transcript::Record]) -> String {
    let (driver, outs) = fold_all(records);
    let mut timeline = Timeline::default();
    let mut folded = Vec::new();
    for (record, out) in records.iter().zip(&outs) {
        for op in &out.ops {
            timeline.apply(op);
        }
        if out.ops.is_empty() && out.events.is_empty() {
            continue;
        }
        let ops: Vec<Value> = out.ops.iter().map(|op| json!(op)).collect();
        let events: Vec<Value> = out.events.iter().map(event_json).collect();
        folded.push(format!(
            "  {{\"seq\": {},\n   \"ops\": {},\n   \"events\": {}}}",
            record.seq,
            lines(&ops, "     "),
            lines(&events, "     "),
        ));
    }
    let items: Vec<Value> = timeline
        .ordered()
        .into_iter()
        .map(|item| json!(item))
        .collect();
    format!(
        "{{\"folded\": [\n{}\n],\n\"timeline\": {},\n\"view\": {}}}\n",
        folded.join(",\n"),
        lines(&items, "  "),
        view_json(driver.view()),
    )
}

/// A JSON array with one compact value per line.
fn lines(values: &[Value], indent: &str) -> String {
    if values.is_empty() {
        return "[]".into();
    }
    let body: Vec<String> = values.iter().map(|v| format!("{indent}{v}")).collect();
    format!("[\n{}\n{}]", body.join(",\n"), &indent[..indent.len() - 2])
}

#[test]
fn every_fixture_matches_its_golden() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let dir = golden_dir();
    if update {
        std::fs::create_dir_all(&dir).expect("golden dir");
    }
    let mut mismatched = Vec::new();
    for (name, records) in fixtures() {
        let rendered = render(&tagged(&records));
        serde_json::from_str::<Value>(&rendered).expect("golden is valid JSON");
        let path = dir.join(format!("{name}.json"));
        if update {
            std::fs::write(&path, &rendered).expect("write golden");
            continue;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_default();
        if expected != rendered {
            mismatched.push(name);
        }
    }
    assert!(
        mismatched.is_empty(),
        "goldens differ (rerun with UPDATE_GOLDEN=1 and review): {mismatched:?}"
    );
}

#[test]
fn every_fixture_ends_with_nothing_open() {
    for (name, records) in fixtures() {
        let (driver, outs) = fold_all(&records);
        let mut timeline = Timeline::default();
        outs.iter()
            .flat_map(|o| &o.ops)
            .for_each(|op| timeline.apply(op));
        let streaming: Vec<_> = timeline
            .ordered()
            .into_iter()
            .filter(|item| item.streaming)
            .map(|item| item.id.0.clone())
            .collect();
        assert!(
            streaming.is_empty(),
            "{name}: still streaming {streaming:?}"
        );
        let view = driver.view();
        assert!(!view.run_active && !view.ready, "{name}: {view:?}");
        assert_eq!(view.open_tools, 0, "{name}");
        assert!(view.open_requests.is_empty(), "{name}");
        assert!(view.unanswered.is_empty(), "{name}");
    }
}
