//! Terminal supervisors (`kind: pty`): byte records, raw input from any connection, resize,
//! job control, and SIGHUP on terminate.

mod support;

use controlroom_supervisor::{Exec, SpawnSpec};
use controlroom_transcript::{Body, ProcessKind, PtyChunk};
use support::{Sup, TIMEOUT, next, records_until};

/// An interactive `sh` on a terminal, with a fixed prompt.
fn shell(spec: &mut SpawnSpec) {
    spec.kind = ProcessKind::Pty;
    spec.exec = Exec::Argv(vec!["/bin/sh".into(), "-i".into()]);
}

/// Bytes of terminal output records until `needle` has appeared (all output so far).
async fn output_until(stream: &mut controlroom_supervisor::RecordStream, needle: &str) -> String {
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !text.contains(needle) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no {needle:?} in {text:?}"
        );
        let record = next(stream).await.expect("stream ended early");
        if let Some(chunk) = PtyChunk::from_record(&record) {
            text.push_str(&String::from_utf8_lossy(&chunk.0));
        }
    }
    text
}

#[tokio::test]
async fn a_terminal_logs_bytes_takes_input_from_anyone_and_resizes() {
    let sup = Sup::start_with("unused", shell);
    let reader = sup.connect(false).await;
    let mut stream = reader.read(1).await.unwrap();

    // Not the writer, yet it may type (terminals take input from every attached client).
    let typist = sup.connect(false).await;
    typist.write_bytes(b"echo pty-$((6*7))\n").await.unwrap();
    output_until(&mut stream, "pty-42").await;

    // A pipe-style write is refused.
    let writer = sup.connect(true).await;
    assert!(writer.write("echo nope").await.is_err());

    typist.resize(100, 33).await.unwrap();
    typist.write_bytes(b"stty size\n").await.unwrap();
    output_until(&mut stream, "33 100").await;

    // The child has the terminal as its controlling terminal: `tty` names it.
    typist.write_bytes(b"tty\n").await.unwrap();
    output_until(&mut stream, "/dev/pts/").await;

    // terminate sends SIGHUP, which an interactive shell honors.
    assert!(!writer.terminate(None).await.unwrap());
    let records = records_until(&mut stream, |r| matches!(r.body, Body::Exit(_))).await;
    let Some(Body::Exit(exit)) = records.last().map(|r| &r.body) else {
        panic!("no exit record");
    };
    assert_eq!(exit.signal, Some(1), "{exit:?}");
    let _ = sup.wait_exit();
}

#[tokio::test]
async fn ctrl_c_interrupts_the_foreground_job_not_the_shell() {
    let sup = Sup::start_with("unused", shell);
    let client = sup.connect(false).await;
    let mut stream = client.read(1).await.unwrap();
    client
        .write_bytes(b"sleep 1000; echo after-$((1+1))\n")
        .await
        .unwrap();
    // Wait until sleep runs as the terminal's foreground job.
    support::wait_until("sleep in the foreground", || {
        foreground_comm(sup.info.child_pid).as_deref() == Some("sleep")
    });
    client.write_bytes(b"\x03").await.unwrap();
    client.write_bytes(b"echo alive-$((2+2))\n").await.unwrap();
    let text = output_until(&mut stream, "alive-4").await;
    assert!(
        !text.contains("after-2"),
        "the shell ran on after ^C: {text:?}"
    );
}

/// The name of the terminal's foreground process group leader, via `/proc/<pid>/stat`.
fn foreground_comm(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let tpgid: i32 = stat
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(5)?
        .parse()
        .ok()?;
    let comm = std::fs::read_to_string(format!("/proc/{tpgid}/comm")).ok()?;
    Some(comm.trim().to_owned())
}
