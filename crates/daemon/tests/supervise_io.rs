//! `controlroomd supervise`: logging, reading, writing, barriers and the line cap.

mod support;

use std::io::Write as _;

use controlroom_supervisor::{Client, ClientError, ErrorCode};
use controlroom_transcript::{Body, read_all};
use serde_json::json;
use support::*;

/// Echoes each stdin line to stdout and stderr.
const ECHO_BOTH: &str = r#"while read -r l; do echo "out:$l"; echo "err:$l" >&2; done"#;

fn code(result: Result<impl std::fmt::Debug, ClientError>) -> ErrorCode {
    match result {
        Err(ClientError::Remote(body)) => body.code,
        other => panic!("expected a remote error, got {other:?}"),
    }
}

#[tokio::test]
async fn records_both_directions_in_order() {
    let sup = Sup::start(ECHO_BOTH);
    let client = sup.connect(true).await;
    let mut stream = client.read(0).await.unwrap();
    assert_eq!(client.write("a").await.unwrap(), 2);
    client.write("b").await.unwrap();
    // stdout and stderr are separate pipes, so their relative order may vary.
    let mut records = records_until(&mut stream, is_out("out:b")).await;
    let is_err_b =
        |r: &controlroom_transcript::Record| r.raw() == Some("err:b") && dir(r) == "stderr";
    if !records.iter().any(is_err_b) {
        records.extend(records_until(&mut stream, is_err_b).await);
    }

    let Body::Meta(meta) = &records[0].body else {
        panic!("first record is not meta: {records:#?}");
    };
    assert_eq!(records[0].seq, 1);
    assert_eq!(meta.id, "s1");
    assert_eq!(meta.argv[1..], ["-lc", ECHO_BOTH]);
    assert_eq!(meta.child_pid, sup.info.child_pid);
    assert_eq!(meta.protocol_version, controlroom_supervisor::PROTOCOL_VERSION);
    assert_eq!(meta.format, controlroom_transcript::FORMAT_VERSION);
    seqs_increase_by_one(&records);
    let pos = |d: &str, raw: &str| {
        records
            .iter()
            .position(|r| dir(r) == d && r.raw() == Some(raw))
            .unwrap_or_else(|| panic!("no {d} {raw} in {records:#?}"))
    };
    let (in_a, out_a, err_a) = (pos("in", "a"), pos("out", "out:a"), pos("stderr", "err:a"));
    let (in_b, out_b) = (pos("in", "b"), pos("out", "out:b"));
    assert!(in_a < out_a && in_a < err_a && in_a < in_b && in_b < out_b && out_a < out_b);

    // The file holds exactly what was streamed.
    let on_disk = read_all(&sup.transcript(), 0).unwrap();
    assert!(on_disk.partial_tail.is_none());
    assert_eq!(on_disk.records[..records.len()], records[..]);
}

#[tokio::test]
async fn read_from_seq_then_live_tail() {
    let sup = Sup::start("cat");
    let writer = sup.connect(true).await;
    let mut first = writer.read(0).await.unwrap();
    for line in ["1", "2", "3"] {
        writer.write(line).await.unwrap();
    }
    let all = records_until(&mut first, is_out("3")).await;
    let from = all[3].seq;

    let reader = sup.connect(false).await;
    let mut stream = reader.read(from).await.unwrap();
    let backlog = records_until(&mut stream, is_out("3")).await;
    assert_eq!(backlog, all[3..]);
    assert_eq!(stream.last_seq(), all.last().unwrap().seq);

    // Live records follow the backlog without gaps.
    writer.write("4").await.unwrap();
    let mut both = backlog;
    both.extend(records_until(&mut stream, is_out("4")).await);
    seqs_increase_by_one(&both);
}

#[tokio::test]
async fn nothing_is_lost_while_no_client_is_attached() {
    let agent = r#"i=0; while [ $i -lt 500 ]; do echo "line $i"; i=$((i+1)); done; while read -r l; do echo "late:$l"; done"#;
    let sup = Sup::start(agent);
    // Write, then detach before the child answers.
    let writer = sup.connect(true).await;
    writer.write("x").await.unwrap();
    drop(writer);

    let reader = sup.connect(false).await;
    let mut stream = reader.read(1).await.unwrap();
    let records = records_until(&mut stream, is_out("late:x")).await;
    seqs_increase_by_one(&records);
    let outs: Vec<&str> = records
        .iter()
        .filter(|r| dir(r) == "out")
        .filter_map(|r| r.raw())
        .collect();
    let expected: Vec<String> = (0..500)
        .map(|i| format!("line {i}"))
        .chain(["late:x".to_owned()])
        .collect();
    assert_eq!(outs, expected);
}

#[tokio::test]
async fn barrier_covers_writes_of_a_dropped_writer() {
    // The child never reads stdin: the barrier must not depend on delivery.
    let sup = Sup::start("exec sleep 600");
    let mut raw = Raw::connect(&sup.paths.socket).await;
    raw.call("hello", json!({"protocol_version": 1, "writer": true}))
        .await;
    for i in 0..50 {
        raw.send("write", json!({"line": format!("w{i}")})).await;
    }
    // Gone without waiting for a single answer, like a killed daemon.
    drop(raw);

    let client = loop {
        match Client::connect(&sup.paths.socket, true).await {
            Ok(client) => break client,
            Err(ClientError::Remote(body)) if body.code == ErrorCode::WriterBusy => {
                // The old connection is still being drained.
                tokio::task::yield_now().await;
            }
            Err(e) => panic!("{e}"),
        }
    };
    let durable = client.barrier().await.unwrap();
    let ins: Vec<_> = read_all(&sup.transcript(), 0)
        .unwrap()
        .records
        .into_iter()
        .filter(|r| dir(r) == "in")
        .collect();
    let lines: Vec<&str> = ins.iter().filter_map(|r| r.raw()).collect();
    let expected: Vec<String> = (0..50).map(|i| format!("w{i}")).collect();
    assert_eq!(lines, expected);
    assert!(ins.iter().all(|r| r.seq <= durable), "barrier {durable}");
    let status = client.status().await.unwrap();
    assert!(status.durable_seq >= durable);

    // Later writes and barriers keep working while the child still doesn't read.
    let seq = client.write("more").await.unwrap();
    assert!(client.barrier().await.unwrap() >= seq);
    assert!(client.sync().await.unwrap() >= seq);
}

#[tokio::test]
async fn long_output_lines_are_truncated_and_long_writes_refused() {
    let sup = Sup::start_with(
        r#"printf 'x%.0s' $(seq 100); printf '\n'; echo short; cat"#,
        |spec| {
            spec.max_line_bytes = Some(16);
        },
    );
    let client = sup.connect(true).await;
    let mut stream = client.read(0).await.unwrap();
    let records = records_until(&mut stream, is_out("short")).await;
    let Body::Meta(meta) = &records[0].body else {
        panic!("no meta");
    };
    assert_eq!(meta.max_line_bytes, 16);
    let long = records[1].line().unwrap();
    assert_eq!(long.raw, "x".repeat(16));
    assert_eq!(long.truncated, 84);

    assert_eq!(
        code(client.write(&"y".repeat(17)).await),
        ErrorCode::LineTooLong
    );
    client.write(&"y".repeat(16)).await.unwrap();
    let echoed = records_until(&mut stream, |r| dir(r) == "out").await;
    assert_eq!(echoed.last().unwrap().raw(), Some("y".repeat(16).as_str()));
}

#[tokio::test]
async fn index_is_correct_after_recovery() {
    let sup = Sup::start("cat");
    let client = sup.connect(true).await;
    let mut stream = client.read(0).await.unwrap();
    for i in 0..300 {
        client.write(&format!("{i}")).await.unwrap();
    }
    records_until(&mut stream, is_out("299")).await;
    client.terminate(None).await.unwrap();
    records_until(&mut stream, is_exit).await;
    drop((client, stream));
    let transcript = sup.transcript();
    let state = sup.wait_exit();

    // A crash left a partial line and a stale index behind.
    let index = controlroom_transcript::index_path(&transcript);
    let entries = std::fs::read(&index).unwrap();
    std::fs::write(&index, &entries[..entries.len() / 3]).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(br#"{"seq":999,"ts":1,"di"#)
        .unwrap();

    // A child that never reads or prints, so nothing is appended after the write.
    let spec = shell_spec(state.path(), "exec sleep 600");
    let sup = Sup::start_in(state, &spec);
    let client = sup.connect(true).await;
    let seq = client.write("again").await.unwrap();
    let on_disk = read_all(&sup.transcript(), 0).unwrap();
    assert!(on_disk.corrupt.is_empty() && on_disk.partial_tail.is_none());
    assert_eq!(on_disk.records.last().unwrap().seq, seq);
    seqs_increase_by_one(&on_disk.records);
    assert_eq!(std::fs::metadata(&index).unwrap().len(), 8 * seq);

    // Reads from any seq match the full file.
    for from in [2, 150, 400, seq - 1, seq] {
        let mut stream = client.read(from).await.unwrap();
        let first = next(&mut stream).await.unwrap();
        let expected = on_disk.records.iter().find(|r| r.seq == from).unwrap();
        assert_eq!(&first, expected, "from {from}");
    }
}

#[tokio::test]
async fn a_job_writes_a_plain_log_with_one_line_per_output_record() {
    let state = tempfile::tempdir().unwrap();
    let log = state.path().join("job.log");
    let script =
        r#"printf '\033[1;31mred\033[0m\n10%%\r50%%\r100%%\r\n'; echo oops >&2; printf 'tail'"#;
    let mut spec = shell_spec(state.path(), script);
    spec.kind = controlroom_transcript::ProcessKind::Job;
    spec.stdin = controlroom_transcript::StdinMode::Null;
    spec.plain_log = Some(log.clone());
    let sup = Sup::start_in(state, &spec);
    let transcript = sup.transcript();
    let _state = sup.wait_exit();

    let records = read_all(&transcript, 0).unwrap().records;
    let output: Vec<&str> = records
        .iter()
        .filter(|r| matches!(dir(r), "out" | "stderr"))
        .filter_map(|r| r.raw())
        .collect();
    assert_eq!(output.len(), 4, "{records:#?}");
    // Line n of the log is output record n (stdout and stderr may interleave either way).
    let plain = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = plain.lines().collect();
    let expected: Vec<String> = output
        .iter()
        .map(|raw| controlroom_supervisor::plain::plain_line(raw).into_owned())
        .collect();
    assert_eq!(lines, expected);
    for line in ["red", "100%", "oops", "tail"] {
        assert!(lines.contains(&line), "{line} in {lines:?}");
    }
    // Output records are seqs 2..=5: the log's line n is seq n + 1.
    assert!(
        records[1..5]
            .iter()
            .all(|r| matches!(dir(r), "out" | "stderr"))
    );
}
