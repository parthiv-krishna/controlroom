//! Checks on the committed fixtures, the mock model, and (ignored by
//! default) a full recording against real pi.

use std::path::{Path, PathBuf};
use std::time::Duration;

use controlroom_fixture_tool::fixture::{self, Action};
use controlroom_fixture_tool::mock_model::{MockModel, Reply};
use controlroom_fixture_tool::sanitize::Sanitizer;
use controlroom_fixture_tool::{scenario, scenarios};
use controlroom_transcript::{Body, FORMAT_VERSION};
use regex::Regex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pi-rpc")
}

fn files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("fixture dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            out.extend(files(&path, ext));
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
    out
}

#[test]
fn every_scenario_has_a_fixture() {
    let dir = fixtures_dir().join("1.2.0");
    for scenario in scenarios::all() {
        let single = dir.join(format!("{}.jsonl", scenario.name));
        let first = dir.join(format!("{}.1.jsonl", scenario.name));
        assert!(
            single.exists() || first.exists(),
            "no fixture for {}",
            scenario.name
        );
    }
}

/// Every fixture loads through the transcript crate with no bad lines, starts
/// with a typed `meta`, ends with `exit`, and has a consistent actions file.
#[test]
fn fixtures_are_well_formed_transcripts() {
    let transcripts = files(&fixtures_dir(), "jsonl");
    assert!(transcripts.len() > 20);
    for path in transcripts {
        let name = path.display();
        let loaded = fixture::read(&path).unwrap_or_else(|e| panic!("{e:#}"));
        let records = &loaded.records;
        let Some(Body::Meta(meta)) = records.first().map(|r| &r.body) else {
            panic!("{name}: meta first");
        };
        assert_eq!(meta.format, FORMAT_VERSION, "{name}");
        assert_eq!(meta.argv[..3], ["pi", "--mode", "rpc"], "{name}");
        assert!(
            matches!(records.last().map(|r| &r.body), Some(Body::Exit(_))),
            "{name}: exit last"
        );
        for (i, record) in records.iter().enumerate() {
            assert_eq!(record.seq, i as u64 + 1, "{name}: seq");
            if let Body::Out(line) = &record.body {
                serde_json::from_str::<serde_json::Value>(&line.raw)
                    .unwrap_or_else(|e| panic!("{name}:{}: {e}", record.seq));
            }
        }
        let last = records.len() as u64;
        assert!(
            loaded.actions.iter().all(|a| a.after_seq() < last),
            "{name}: actions after the exit"
        );
        assert!(
            loaded
                .actions
                .windows(2)
                .all(|w| w[0].after_seq() <= w[1].after_seq()),
            "{name}: actions out of order"
        );
        let closes = loaded
            .actions
            .iter()
            .filter(|a| matches!(a, Action::CloseStdin { .. } | Action::Signal { .. }))
            .count();
        assert!(closes <= 1, "{name}: more than one stop action");
    }
}

/// Nothing machine-specific survives sanitizing.
#[test]
fn fixtures_carry_no_machine_details() {
    let forbidden = [
        (r"/tmp/", "temp paths"),
        (r"cr-fixture-[A-Za-z0-9]{6}", "the recording temp dir"),
        (r"/nix/store/[0-9a-z]{32}", "Nix store hashes"),
        (r"/home/[^a/]", "home dirs other than /home/alice"),
        (
            r"127\.0\.0\.1:(?:[0-35-9]\d*|4\d{4,}|4[1-9]\d*)",
            "mock ports other than 4000",
        ),
        (
            r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
            "unmapped UUIDs",
        ),
    ];
    let zero_store = Regex::new(r"/nix/store/0{32}").expect("regex");
    let placeholder = Regex::new(r"00000000-0000-4000-8000-\d{12}").expect("regex");
    for path in files(&fixtures_dir(), "jsonl") {
        let text = std::fs::read_to_string(&path).expect("read");
        let text = zero_store.replace_all(&text, "");
        let text = placeholder.replace_all(&text, "");
        for (pattern, what) in forbidden {
            let re = Regex::new(pattern).expect("regex");
            if let Some(m) = re.find(&text) {
                panic!("{}: {what}: {:?}", path.display(), m.as_str());
            }
        }
    }
}

/// The mock streams up to a gate, holds, then finishes after release.
#[tokio::test]
async fn mock_model_streams_and_holds_at_gates() {
    let mock = MockModel::start(vec![
        Reply::gated_text("hello", "g1", " world"),
        Reply::error(503, "busy"),
    ])
    .await
    .expect("mock");
    let body = r#"{"model":"mock-model","stream":true,"messages":[]}"#;
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", mock.port()))
        .await
        .expect("connect");
    stream.write_all(request.as_bytes()).await.expect("write");
    mock.wait_reached("g1", Duration::from_secs(10))
        .await
        .expect("gate reached");
    mock.release("g1");
    let mut response = String::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.read_to_string(&mut response),
    )
    .await
    .expect("response in time")
    .expect("read");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains(r#""content":"hello""#));
    assert!(response.contains(r#""content":" world""#));
    assert!(response.contains(r#""finish_reason":"stop""#));
    assert!(response.contains("data: [DONE]"));

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", mock.port()))
        .await
        .expect("connect");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("read");
    assert!(response.starts_with("HTTP/1.1 503"), "{response}");
    assert_eq!(mock.requests().len(), 2);
    assert_eq!(mock.remaining(), 0);
}

/// Records every scenario with the real pi on PATH (or `$CONTROLROOM_PI`),
/// sanitizes the result and checks it against the committed fixtures,
/// ignoring only timing and pi's known races (see [`timeless`]). Needs pi
/// 1.2.0; offline (mock model only).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real pi binary"]
async fn records_every_scenario_against_real_pi() {
    let sanitizer = Sanitizer::for_this_machine();
    let dir = fixtures_dir().join("1.2.0");
    let mut differences = Vec::new();
    for scenario in scenarios::all() {
        let recordings = scenario::run(&scenario)
            .await
            .unwrap_or_else(|e| panic!("{}: {e:#}", scenario.name));
        let transcripts: Vec<_> = recordings.iter().map(|(_, f)| f.records.clone()).collect();
        let clean = sanitizer.scenario(&transcripts);
        let unordered = UNORDERED.contains(&scenario.name);
        for ((name, recorded), records) in recordings.iter().zip(clean) {
            let committed = fixture::read(&dir.join(name)).expect("committed fixture");
            if recorded.actions != committed.actions {
                differences.push(format!("{name}: actions differ"));
            }
            let mut new: Vec<_> = records.iter().map(|r| timeless(r, unordered)).collect();
            let mut old: Vec<_> = committed
                .records
                .iter()
                .map(|r| timeless(r, unordered))
                .collect();
            if unordered {
                new.sort();
                old.sort();
            }
            if let Some(i) = (0..new.len().max(old.len())).find(|&i| new.get(i) != old.get(i)) {
                differences.push(format!(
                    "{name}: record {}:\n  new: {:?}\n  old: {:?}",
                    i + 1,
                    new.get(i),
                    old.get(i)
                ));
            }
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

/// Scenarios whose record order pi does not fix (parallel tools finish in
/// any order); they are compared as sorted lists without seqs.
const UNORDERED: &[&str] = &["parallel-tools"];

/// Keys whose values depend on timing: pi omits `durationMs` when an aborted
/// request never started, and `estimatedTokensAfter` is a heuristic over
/// content that includes such values.
const TIMING_KEYS: &[&str] = &["durationMs", "wall_time_seconds", "estimatedTokensAfter"];

/// A record without timing: no `ts` (nor `seq` when `unordered`); in JSON
/// lines, `timestamp` values and [`TIMING_KEYS`] are dropped, as are the
/// live snapshots pi serializes while a stream is parsed (the content of
/// assistant `message_start`, the cumulative `usage` of `message_update`),
/// which can already be ahead; ISO times are blanked.
fn timeless(record: &controlroom_transcript::Record, unordered: bool) -> String {
    let iso = Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}[:-]\d{2}[:-]\d{2}[.-]\d{3}Z").expect("regex");
    let mut value = serde_json::to_value(record).expect("record");
    value["ts"] = serde_json::Value::Null;
    if unordered {
        value["seq"] = serde_json::Value::Null;
    }
    if let Some(raw) = record.raw()
        && let Ok(mut line) = serde_json::from_str::<serde_json::Value>(raw)
    {
        drop_timing(&mut line);
        if line["type"] == "message_start" && line["message"]["role"] == "assistant" {
            line["message"]["content"] = serde_json::Value::Null;
        }
        if line["type"] == "message_update" {
            line["usage"] = serde_json::Value::Null;
        }
        value["raw"] = line;
    }
    iso.replace_all(&value.to_string(), "<time>").into_owned()
}

fn drop_timing(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.retain(|key, _| !TIMING_KEYS.contains(&key.as_str()));
            for (key, v) in map.iter_mut() {
                if key == "timestamp" {
                    *v = serde_json::Value::Null;
                } else {
                    drop_timing(v);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(drop_timing),
        _ => {}
    }
}
