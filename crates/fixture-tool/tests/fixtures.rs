//! Checks on the committed fixtures, the mock model, and (ignored by
//! default) a full recording against real pi.

use std::path::{Path, PathBuf};
use std::time::Duration;

use controlroom_fixture_tool::mock_model::{MockModel, Reply};
use controlroom_fixture_tool::record::{Dir, read_jsonl};
use controlroom_fixture_tool::sanitize::Sanitizer;
use controlroom_fixture_tool::{scenario, scenarios};
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

#[test]
fn fixtures_are_well_formed_records() {
    let transcripts = files(&fixtures_dir(), "jsonl");
    assert!(transcripts.len() > 20);
    for path in transcripts {
        let records = read_jsonl(&path).expect("records");
        assert_eq!(
            records[0].dir,
            Dir::Meta,
            "{}: env meta first",
            path.display()
        );
        assert_eq!(
            records.last().map(|r| r.dir),
            Some(Dir::Exit),
            "{}",
            path.display()
        );
        for (i, record) in records.iter().enumerate() {
            assert_eq!(record.seq, i as u64 + 1, "{}: seq", path.display());
            if matches!(record.dir, Dir::Out | Dir::Meta | Dir::Exit) {
                serde_json::from_str::<serde_json::Value>(&record.raw)
                    .unwrap_or_else(|e| panic!("{}:{}: {e}", path.display(), record.seq));
            }
        }
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

/// Records every scenario with the real pi on PATH (or `$CONTROLROOM_PI`)
/// and sanitizes the result. Needs pi 1.2.0; offline (mock model only).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real pi binary"]
async fn records_every_scenario_against_real_pi() {
    let sanitizer = Sanitizer::for_this_machine();
    for scenario in scenarios::all() {
        let recordings = scenario::run(&scenario)
            .await
            .unwrap_or_else(|e| panic!("{}: {e:#}", scenario.name));
        let raw: Vec<_> = recordings.into_iter().map(|(_, records)| records).collect();
        let clean = sanitizer.scenario(&raw);
        assert!(
            clean
                .iter()
                .all(|t| t.last().map(|r| r.dir) == Some(Dir::Exit))
        );
    }
}
