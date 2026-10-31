//! Conformance: replay every recorded pi transcript in
//! `fixtures/pi-rpc/1.2.0` against fake-agent and compare the event
//! sequences.
//!
//! Each fixture `<name>.jsonl` (or `<name>.N.jsonl` for scenarios that run
//! several processes) has a fake scenario `fake/<name>.json` that scripts
//! the same model replies and extension behavior. The replay sends the
//! fixture's stdin lines at the same points (after as many stdout records
//! as pi had produced), performs the recorder's actions from
//! `<name>.actions.json` (gate releases, signals, stdin close) at the same
//! points, and finally compares a signature of
//! every stdout record (type, the discriminating fields, top-level keys)
//! and the exit status.

mod support;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use controlroom_transcript::{Body, Record, read_all};
use serde_json::Value;
use support::{Sandbox, fixtures_dir};

/// Fixtures without a fake replay, and why.
const EXCLUDED: &[(&str, &str)] = &[
    (
        "parallel-tools",
        "pi runs the two tools concurrently; their end order varies between runs",
    ),
    (
        "rpc-bash",
        "fake-agent does not implement the RPC bash command",
    ),
];

/// Keys whose string values are ids that differ between pi and the fake.
const ID_KEYS: &[&str] = &[
    "id",
    "parentId",
    "leafId",
    "targetId",
    "firstKeptEntryId",
    "sessionId",
    "sessionFile",
];

/// A recorded pi process: its transcript and the recorder's actions.
struct Fixture {
    records: Vec<Record>,
    /// `<name>.actions.json`: `{"action", "after_seq", ...}` objects.
    actions: Vec<Value>,
}

/// Load a fixture through the transcript crate; bad lines are an error.
fn read_fixture(path: &Path) -> Fixture {
    let all = read_all(path, 0).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    assert!(
        all.corrupt.is_empty() && all.partial_tail.is_none(),
        "{}: bad records {:?}",
        path.display(),
        all.corrupt
    );
    let actions_file = path.with_extension("actions.json");
    let actions = std::fs::read_to_string(&actions_file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| panic!("{}: missing or bad", actions_file.display()));
    Fixture {
        records: all.records,
        actions,
    }
}

/// A comparable summary of one stdout record.
fn signature(v: &Value) -> String {
    let kind = v["type"].as_str().unwrap_or("?");
    let flag = |key: &str| v.get(key).map_or(String::new(), |x| format!(" {key}={x}"));
    let detail = match kind {
        "response" => format!(
            "{} success={}{}",
            v["command"],
            v["success"],
            v["data"]
                .get("disposition")
                .map_or(String::new(), |d| format!(" {d}"))
        ),
        "message_update" => v["assistantMessageEvent"]["type"].to_string(),
        "message_start" | "message_end" | "turn_end" => {
            format!("{} {}", v["message"]["role"], v["message"]["stopReason"])
        }
        "extension_ui_request" => v["method"].to_string(),
        "tool_execution_start" | "tool_execution_update" | "tool_execution_end" => format!(
            "{}{} nested={}",
            v["toolName"],
            flag("isError"),
            v.get("parentToolCallId").is_some()
        ),
        "compaction_start" | "compaction_end" => {
            format!("{} error={}", v["reason"], v.get("errorMessage").is_some())
        }
        "queue_update" => format!(
            "steering={} followUp={}",
            v["steering"].as_array().map_or(0, Vec::len),
            v["followUp"].as_array().map_or(0, Vec::len)
        ),
        "entry_appended" => format!("{} {}", v["entry"]["type"], v["entry"]["customType"]),
        _ => [
            "willRetry",
            "aborted",
            "success",
            "level",
            "name",
            "attempt",
        ]
        .iter()
        .map(|k| flag(k))
        .collect(),
    };
    let mut keys: Vec<&str> = v
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    format!("{kind} {detail} {keys:?}")
}

/// Learn id correspondences from two aligned records.
fn learn(ids: &mut HashMap<String, String>, pi: &Value, fake: &Value) {
    match (pi, fake) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, va) in a {
                let Some(vb) = b.get(key) else { continue };
                if let (true, Value::String(sa), Value::String(sb)) =
                    (ID_KEYS.contains(&key.as_str()), va, vb)
                    && sa != sb
                {
                    ids.insert(sa.clone(), sb.clone());
                }
                learn(ids, va, vb);
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for (va, vb) in a.iter().zip(b) {
                learn(ids, va, vb);
            }
        }
        _ => {}
    }
}

/// Replace pi's ids with the fake's in a value.
fn rewrite(ids: &HashMap<String, String>, value: &mut Value) {
    match value {
        Value::String(s) => {
            if let Some(mapped) = ids.get(s.as_str()) {
                *s = mapped.clone();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| rewrite(ids, v)),
        Value::Object(map) => map.values_mut().for_each(|v| rewrite(ids, v)),
        _ => {}
    }
}

/// Learn ids from the aligned stdout records `from..to`; returns `to`.
fn learn_until(
    fake: &support::Fake,
    ids: &mut HashMap<String, String>,
    pi_out: &[Value],
    from: usize,
    to: usize,
) -> usize {
    let records = fake.records();
    for i in from..to.min(records.len()) {
        learn(ids, &pi_out[i], &records[i]);
    }
    to
}

/// Replay one transcript; returns pi's and the fake's signatures.
async fn replay(
    sandbox: &Sandbox,
    scenario: &str,
    fixture: &Fixture,
    ids: &mut HashMap<String, String>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let json = |raw: &str| serde_json::from_str::<Value>(raw).ok();
    let pi_out: Vec<Value> = fixture
        .records
        .iter()
        .filter_map(|r| match &r.body {
            Body::Out(line) => json(&line.raw),
            _ => None,
        })
        .collect();
    let Some(Body::Meta(meta)) = fixture.records.first().map(|r| &r.body) else {
        return Err("fixture does not start with a meta record".into());
    };
    // `argv` is `pi --mode rpc <args>`; the fake gets the same args.
    let args: Vec<String> = meta
        .argv
        .iter()
        .skip(3)
        .map(|a| ids.get(a).cloned().unwrap_or_else(|| a.clone()))
        .collect();
    let gates = sandbox.gates();
    let mut fake = sandbox.spawn(scenario, &args);

    let mut expected = 0;
    let mut learned = 0;
    let mut exit = None;
    let mut actions = fixture.actions.iter().peekable();
    for record in &fixture.records {
        match &record.body {
            Body::Out(line) if json(&line.raw).is_some() => expected += 1,
            Body::In(line) => {
                fake.wait_count(expected).await?;
                learned = learn_until(&fake, ids, &pi_out, learned, expected);
                match json(&line.raw) {
                    Some(mut command) => {
                        rewrite(ids, &mut command);
                        fake.send(command).await;
                    }
                    None => fake.send_raw(&line.raw).await,
                }
            }
            Body::Exit(info) => exit = Some(*info),
            _ => {}
        }
        while let Some(action) = actions.next_if(|a| a["after_seq"].as_u64() == Some(record.seq)) {
            fake.wait_count(expected).await?;
            learned = learn_until(&fake, ids, &pi_out, learned, expected);
            let text = |key: &str| action[key].as_str().unwrap_or_default();
            match text("action") {
                "release_gate" => gates.release(text("gate")).map_err(|e| e.to_string())?,
                "signal" => fake.signal(text("signal")),
                "close_stdin" => fake.close_stdin(),
                other => return Err(format!("unknown action {other:?}")),
            }
        }
    }
    if let Some(action) = actions.next() {
        return Err(format!("action after the last record: {action}"));
    }
    let (code, signal) = fake.wait_exit().await;
    let records = fake.records();
    for i in learned..pi_out.len().min(records.len()) {
        learn(ids, &pi_out[i], &records[i]);
    }
    let show = |x: Option<i32>| x.map_or("null".to_owned(), |x| x.to_string());
    let mut pi_sigs: Vec<String> = pi_out.iter().map(signature).collect();
    let mut fake_sigs: Vec<String> = records.iter().map(signature).collect();
    if let Some(exit) = exit {
        pi_sigs.push(format!(
            "exit code={} signal={}",
            show(exit.code),
            show(exit.signal)
        ));
        fake_sigs.push(format!("exit code={} signal={}", show(code), show(signal)));
    }
    Ok((pi_sigs, fake_sigs))
}

/// A readable report of the first difference.
fn diff(pi: &[String], fake: &[String]) -> Option<String> {
    let first = (0..pi.len().max(fake.len())).find(|&i| pi.get(i) != fake.get(i))?;
    let start = first.saturating_sub(3);
    let mut report = format!("first difference at record {first}:\n");
    for i in start..(first + 4).min(pi.len().max(fake.len())) {
        let none = "<none>".to_owned();
        report.push_str(&format!(
            "  {i:3} pi:   {}\n      fake: {}\n",
            pi.get(i).unwrap_or(&none),
            fake.get(i).unwrap_or(&none)
        ));
    }
    Some(report)
}

/// Fixture files grouped by scenario name (`name.jsonl`, `name.N.jsonl`).
fn fixture_groups() -> BTreeMap<String, Vec<std::path::PathBuf>> {
    let mut groups: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for entry in std::fs::read_dir(fixtures_dir()).expect("fixtures dir") {
        let path = entry.expect("dir entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(stem) = name.strip_suffix(".jsonl") {
            let group = stem.split('.').next().unwrap_or(stem).to_owned();
            groups.entry(group).or_default().push(path);
        }
    }
    for paths in groups.values_mut() {
        paths.sort();
    }
    groups
}

/// The fake scenario for one fixture file: `fake/<name>.N.json` for one
/// process of a multi-process scenario, else `fake/<name>.json`.
fn fake_scenario(group: &str, file: &str) -> Result<String, String> {
    let dir = fixtures_dir().join("fake");
    let own = dir.join(file.replace(".jsonl", ".json"));
    let shared = dir.join(format!("{group}.json"));
    std::fs::read_to_string(&own)
        .or_else(|_| std::fs::read_to_string(&shared))
        .map_err(|_| format!("{file}: no fake scenario at {}", shared.display()))
}

#[tokio::test(flavor = "multi_thread")]
async fn fake_agent_reproduces_pi_fixtures() {
    let groups = fixture_groups();
    assert!(groups.len() > 20, "fixtures missing: {groups:?}");
    let mut tasks = Vec::new();
    for (name, paths) in groups {
        if EXCLUDED.iter().any(|(excluded, _)| *excluded == name) {
            continue;
        }
        tasks.push(tokio::spawn(async move {
            let sandbox = Sandbox::new();
            let mut ids = HashMap::new();
            let mut failures = Vec::new();
            for path in &paths {
                let file = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let scenario = match fake_scenario(&name, &file) {
                    Ok(scenario) => scenario,
                    Err(err) => {
                        failures.push(err);
                        continue;
                    }
                };
                match replay(&sandbox, &scenario, &read_fixture(path), &mut ids).await {
                    Ok((pi, fake)) => {
                        if let Some(report) = diff(&pi, &fake) {
                            failures.push(format!("{file}: {report}"));
                        }
                    }
                    Err(err) => failures.push(format!("{file}: {err}")),
                }
            }
            failures
        }));
    }
    let mut failures = Vec::new();
    for task in tasks {
        failures.extend(task.await.expect("replay task"));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_fake_scenario_parses() {
    let dir = fixtures_dir().join("fake");
    for entry in std::fs::read_dir(&dir).expect("fake scenario dir") {
        let path = entry.expect("dir entry").path();
        let text = std::fs::read_to_string(&path).expect("read scenario");
        if let Err(err) = controlroom_fake_agent::Scenario::from_json(&text) {
            panic!("{}: {err:#}", path.display());
        }
    }
}
