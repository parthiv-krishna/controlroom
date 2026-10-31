//! Conformance: replay every recorded pi transcript in
//! `fixtures/pi-rpc/1.2.0` against fake-agent and compare the event
//! sequences.
//!
//! Each fixture `<name>.jsonl` (or `<name>.N.jsonl` for scenarios that run
//! several processes) has a fake scenario `fake/<name>.json` that scripts
//! the same model replies and extension behavior. The replay sends the
//! fixture's stdin lines at the same points (after as many stdout records
//! as pi had produced), releases gates where the recorder released the
//! mock model, sends the same signals, and finally compares a signature of
//! every stdout record (type, the discriminating fields, top-level keys)
//! and the exit status.

mod support;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

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

/// One fixture record.
struct Record {
    dir: String,
    raw: String,
}

fn read_fixture(path: &Path) -> Vec<Record> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let v: Value = serde_json::from_str(line).expect("fixture record");
            Record {
                dir: v["dir"].as_str().unwrap_or_default().to_owned(),
                raw: v["raw"].as_str().unwrap_or_default().to_owned(),
            }
        })
        .collect()
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

/// Replay one transcript; returns pi's and the fake's signatures.
async fn replay(
    sandbox: &Sandbox,
    scenario: &str,
    records: &[Record],
    ids: &mut HashMap<String, String>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let pi_out: Vec<Value> = records
        .iter()
        .filter(|r| r.dir == "out")
        .filter_map(|r| serde_json::from_str(&r.raw).ok())
        .collect();
    let env = records
        .iter()
        .find(|r| r.dir == "meta")
        .and_then(|r| serde_json::from_str::<Value>(&r.raw).ok())
        .ok_or("fixture has no env meta record")?;
    let args: Vec<String> = env["args"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|a| ids.get(a).cloned().unwrap_or_else(|| a.to_owned()))
        .collect();
    let gates = sandbox.gates();
    let mut fake = sandbox.spawn(scenario, &args);

    let mut expected = 0;
    let mut learned = 0;
    let mut exit = None;
    for record in records {
        let sync = |fake: &mut support::Fake, ids: &mut HashMap<String, String>, n: usize| {
            let records = fake.records();
            for i in learned..n.min(records.len()) {
                learn(ids, &pi_out[i], &records[i]);
            }
            n
        };
        match record.dir.as_str() {
            "out" if serde_json::from_str::<Value>(&record.raw).is_ok() => expected += 1,
            "in" => {
                fake.wait_count(expected).await?;
                learned = sync(&mut fake, ids, expected);
                match serde_json::from_str::<Value>(&record.raw) {
                    Ok(mut command) => {
                        rewrite(ids, &mut command);
                        fake.send(command).await;
                    }
                    Err(_) => fake.send_raw(&record.raw).await,
                }
            }
            "meta" => {
                let meta: Value = serde_json::from_str(&record.raw).unwrap_or_default();
                if meta["fixture"] == "env" {
                    continue;
                }
                fake.wait_count(expected).await?;
                learned = sync(&mut fake, ids, expected);
                match meta["fixture"].as_str() {
                    Some("release_gate") => gates
                        .release(meta["gate"].as_str().unwrap_or_default())
                        .map_err(|e| e.to_string())?,
                    Some("signal") => fake.signal(meta["signal"].as_str().unwrap_or_default()),
                    Some("close_stdin") => fake.close_stdin(),
                    other => return Err(format!("unknown meta record {other:?}")),
                }
            }
            "exit" => exit = serde_json::from_str::<Value>(&record.raw).ok(),
            _ => {}
        }
    }
    let (code, signal) = fake.wait_exit().await;
    let records = fake.records();
    for i in learned..pi_out.len().min(records.len()) {
        learn(ids, &pi_out[i], &records[i]);
    }
    let mut pi_sigs: Vec<String> = pi_out.iter().map(signature).collect();
    let mut fake_sigs: Vec<String> = records.iter().map(signature).collect();
    if let Some(exit) = exit {
        pi_sigs.push(format!(
            "exit code={} signal={}",
            exit["code"], exit["signal"]
        ));
        let show = |x: Option<i32>| x.map_or("null".to_owned(), |x| x.to_string());
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
