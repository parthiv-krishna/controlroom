//! `controlroomd supervise`: exits, signals, timeouts, process groups, detaching.

mod support;

use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use controlroom_supervisor::{
    Client, ClientError, ErrorCode, Exec, LaunchRequest, ReadyLine, SupervisorInfo, launch,
    read_info,
};
use controlroom_transcript::{Body, ExitInfo, ProcessKind, StdinMode, StopCause, read_all};
use rustix::process::Signal;
use support::*;

fn exit_of(record: &controlroom_transcript::Record) -> ExitInfo {
    match record.body {
        Body::Exit(exit) => exit,
        _ => panic!("not an exit record: {record:?}"),
    }
}

fn last_exit(sup_transcript: &std::path::Path) -> ExitInfo {
    let all = read_all(sup_transcript, 0).unwrap();
    exit_of(all.records.last().unwrap())
}

#[tokio::test]
async fn child_crash_writes_exit_record_and_cleans_up() {
    let sup = Sup::start(r#"read -r l; echo "got $l"; kill -SEGV $$"#);
    let client = sup.connect(true).await;
    let mut stream = client.read(0).await.unwrap();
    client.write("x").await.unwrap();
    let records = records_until(&mut stream, is_exit).await;
    assert!(records.iter().any(is_out("got x")));
    let exit = exit_of(records.last().unwrap());
    assert_eq!((exit.code, exit.signal, exit.cause), (None, Some(11), None));
    // The stream ends after the exit record.
    assert!(next(&mut stream).await.is_none());
    // Nothing more can be written.
    assert!(client.write("y").await.is_err());
    drop((client, stream));

    let transcript = sup.transcript();
    let _state = sup.wait_exit();
    assert_eq!(last_exit(&transcript).signal, Some(11));
}

#[test]
fn exit_code_is_recorded_without_any_client() {
    let sup = Sup::start("echo bye; exit 3");
    let transcript = sup.transcript();
    let _state = sup.wait_exit();
    let all = read_all(&transcript, 0).unwrap();
    let raws: Vec<_> = all.records.iter().filter_map(|r| r.raw()).collect();
    assert_eq!(raws, ["bye"]);
    let exit = last_exit(&transcript);
    assert_eq!((exit.code, exit.signal, exit.cause), (Some(3), None, None));
}

#[tokio::test]
async fn terminate_sends_sigterm() {
    let sup = Sup::start("cat");
    let client = sup.connect(true).await;
    let mut stream = client.read(0).await.unwrap();
    let already = client
        .terminate(Some(Duration::from_secs(10)))
        .await
        .unwrap();
    assert!(!already);
    let exit = exit_of(records_until(&mut stream, is_exit).await.last().unwrap());
    assert_eq!(exit.signal, Some(15));
    assert_eq!(exit.cause, Some(StopCause::Terminate));
}

#[tokio::test]
async fn terminate_escalates_to_sigkill() {
    // The ignored TERM disposition survives the exec.
    let sup = Sup::start(r#"trap "" TERM; echo ready; exec sleep 600"#);
    let client = sup.connect(true).await;
    let mut stream = client.read(0).await.unwrap();
    records_until(&mut stream, is_out("ready")).await;
    let started = Instant::now();
    client
        .terminate(Some(Duration::from_millis(300)))
        .await
        .unwrap();
    let exit = exit_of(records_until(&mut stream, is_exit).await.last().unwrap());
    assert_eq!(exit.signal, Some(9));
    assert_eq!(exit.cause, Some(StopCause::Terminate));
    assert!(started.elapsed() >= Duration::from_millis(300));
    drop((client, stream));
    let _state = sup.wait_exit();
}

#[tokio::test]
async fn sigterm_to_supervisor_terminates_child_and_sighup_is_ignored() {
    let sup = Sup::start("cat");
    let pid = sup.child.id();
    let client = sup.connect(false).await;
    let mut stream = client.read(0).await.unwrap();

    signal(pid, Signal::HUP);
    assert!(client.status().await.unwrap().alive);
    signal(pid, Signal::TERM);
    let exit = exit_of(records_until(&mut stream, is_exit).await.last().unwrap());
    assert_eq!(exit.signal, Some(15));
    assert_eq!(exit.cause, Some(StopCause::Signal));
    drop((client, stream));
    let _state = sup.wait_exit();
}

#[tokio::test]
async fn job_timeout_terminates_the_group_and_is_recorded() {
    let sup = Sup::start_with("echo started; exec sleep 600", |spec| {
        spec.kind = ProcessKind::Job;
        spec.stdin = StdinMode::Null;
        spec.timeout_ms = Some(200);
    });
    assert_eq!(sup.info.kind, ProcessKind::Job);
    let client = sup.connect(false).await;
    let mut stream = client.read(0).await.unwrap();
    let records = records_until(&mut stream, is_exit).await;
    let Body::Meta(meta) = &records[0].body else {
        panic!("no meta");
    };
    assert_eq!((meta.kind, meta.timeout_ms), (ProcessKind::Job, Some(200)));
    assert!(records.iter().any(is_out("started")));
    let exit = exit_of(records.last().unwrap());
    assert_eq!(exit.signal, Some(15));
    assert_eq!(exit.cause, Some(StopCause::Timeout));
    drop((client, stream));
    let _state = sup.wait_exit();
}

#[tokio::test]
async fn leftover_group_members_are_killed_when_the_child_exits() {
    let sup = Sup::start("sleep 600 & echo $!; exit 0");
    // The child exits at once, so the supervisor may be gone before a reader could connect:
    // read the transcript it leaves behind.
    let transcript = sup.transcript();
    let _state = sup.wait_exit();
    let records = read_all(&transcript, 0).unwrap().records;
    let leftover: u32 = records
        .iter()
        .find(|r| dir(r) == "out")
        .and_then(|r| r.raw())
        .and_then(|raw| raw.parse().ok())
        .expect("background pid");
    assert_eq!(exit_of(records.last().unwrap()).code, Some(0));
    wait_until("the background process to die", || is_dead(leftover));
}

#[tokio::test]
async fn wrapper_argv_passes_io_and_signals_through() {
    let state = tempfile::tempdir().unwrap();
    let shell = login_shell(state.path()).to_string_lossy().into_owned();
    // Like `nono run ... -- $SHELL -lc '<cmd>'`.
    let argv: Vec<String> = [
        "/bin/sh",
        "-c",
        r#"echo wrapper-start >&2; exec "$@""#,
        "wrapper",
        &shell,
        "-lc",
        "cat",
    ]
    .map(str::to_owned)
    .into();
    let mut spec = shell_spec(state.path(), "unused");
    spec.exec = Exec::Argv(argv.clone());
    spec.labels.insert("driver".into(), "pi".into());
    let sup = Sup::start_in(state, &spec);

    let client = sup.connect(true).await;
    let mut stream = client.read(0).await.unwrap();
    client.write("through").await.unwrap();
    let records = records_until(&mut stream, is_out("through")).await;
    let Body::Meta(meta) = &records[0].body else {
        panic!("no meta");
    };
    assert_eq!(meta.argv, argv);
    assert_eq!(meta.labels["driver"], "pi");
    client.terminate(None).await.unwrap();
    let records = records_until(&mut stream, is_exit).await;
    assert_eq!(exit_of(records.last().unwrap()).signal, Some(15));
    drop((client, stream));
    let transcript = sup.transcript();
    let _state = sup.wait_exit();
    let all = read_all(&transcript, 0).unwrap();
    assert!(all.records.iter().any(|r| r.raw() == Some("wrapper-start")));
}

#[tokio::test]
async fn null_stdin_refuses_writes() {
    let sup = Sup::start_with("echo ready; exec sleep 600", |spec| {
        spec.kind = ProcessKind::Job;
        spec.stdin = StdinMode::Null;
    });
    let client = sup.connect(true).await;
    match client.write("x").await {
        Err(ClientError::Remote(body)) => assert_eq!(body.code, ErrorCode::NoStdin),
        other => panic!("{other:?}"),
    }
    client.barrier().await.unwrap();
    client.terminate(None).await.unwrap();
    drop(client);
    let _state = sup.wait_exit();

    // A child reading a null stdin sees EOF at once.
    let sup = Sup::start_with("cat; echo done", |spec| spec.stdin = StdinMode::Null);
    let transcript = sup.transcript();
    let _state = sup.wait_exit();
    let raws: Vec<_> = read_all(&transcript, 0)
        .unwrap()
        .records
        .iter()
        .filter_map(|r| r.raw().map(str::to_owned))
        .collect();
    assert_eq!(raws, ["done"]);
}

#[test]
fn env_file_reaches_the_child() {
    let state = tempfile::tempdir().unwrap();
    let env_file = state.path().join("env");
    std::fs::write(&env_file, "# test\nCR_TEST_VALUE=hello world\n").unwrap();
    let mut spec = shell_spec(state.path(), r#"echo "$CR_TEST_VALUE""#);
    spec.env_file = Some(env_file);
    let sup = Sup::start_in(state, &spec);
    let transcript = sup.transcript();
    let _state = sup.wait_exit();
    let all = read_all(&transcript, 0).unwrap();
    assert!(all.records.iter().any(|r| r.raw() == Some("hello world")));
}

#[tokio::test]
async fn restart_appends_to_an_existing_transcript() {
    let sup = Sup::start("exit 0");
    let transcript = sup.transcript();
    let state = sup.wait_exit();
    // A crash left a truncated line behind.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(br#"{"seq":3,"ts":1,"di"#)
        .unwrap();

    let spec = shell_spec(state.path(), "cat");
    let sup = Sup::start_in(state, &spec);
    let client = sup.connect(true).await;
    let mut stream = client.read(0).await.unwrap();
    client.write("again").await.unwrap();
    let records = records_until(&mut stream, is_out("again")).await;
    seqs_increase_by_one(&records);
    let dirs: Vec<&str> = records.iter().map(dir).collect();
    assert_eq!(dirs, ["meta", "exit", "meta", "in", "out"]);
    assert_eq!(exit_of(&records[1]).code, Some(0));
}

#[tokio::test]
async fn second_supervisor_for_the_same_dir_is_refused() {
    let sup = Sup::start("cat");
    let spec = shell_spec(sup.state.path(), "cat");
    // Same directory, different socket.
    let mut paths = sup.paths.clone();
    paths.socket = sup.state.path().join("other.sock");
    let (mut child, ready) = run_foreground(sup.state.path(), &paths, "s1", &spec);
    let ReadyLine::Error(message) = ready else {
        panic!("second supervisor started");
    };
    assert!(
        message.contains("already has a live supervisor"),
        "{message}"
    );
    assert!(!wait_child(&mut child).success());
    // The first one is unaffected.
    let client = sup.connect(false).await;
    assert!(client.status().await.unwrap().alive);
}

#[tokio::test]
async fn live_socket_is_never_replaced() {
    let sup = Sup::start("cat");
    let other = tempfile::tempdir().unwrap();
    let spec = shell_spec(other.path(), "cat");
    let mut paths = paths(other.path(), "s2");
    paths.socket = sup.paths.socket.clone();
    let (mut child, ready) = run_foreground(other.path(), &paths, "s2", &spec);
    let ReadyLine::Error(message) = ready else {
        panic!("second supervisor took the socket");
    };
    assert!(message.contains("in use"), "{message}");
    assert!(!wait_child(&mut child).success());
    let client = sup.connect(false).await;
    assert_eq!(client.hello().id, "s1");
}

#[test]
fn bad_arguments_are_refused_clearly() {
    let state = tempfile::tempdir().unwrap();
    let spec = shell_spec(state.path(), "cat");
    let mut long = paths(state.path(), "s1");
    long.socket = state.path().join("x".repeat(110));
    let (mut child, ready) = run_foreground(state.path(), &long, "s1", &spec);
    let ReadyLine::Error(message) = ready else {
        panic!("started with an overlong socket path");
    };
    assert!(message.contains("107-byte unix socket limit"), "{message}");
    assert!(!wait_child(&mut child).success());

    let mut unknown = spec.clone();
    unknown.kind = ProcessKind::Unknown;
    let (mut child, ready) =
        run_foreground(state.path(), &paths(state.path(), "s1"), "s1", &unknown);
    assert!(matches!(ready, ReadyLine::Error(m) if m.contains("unsupported process kind")));
    assert!(!wait_child(&mut child).success());

    let (mut child, ready) =
        run_foreground(state.path(), &paths(state.path(), "a/b"), "a/b", &spec);
    assert!(matches!(ready, ReadyLine::Error(m) if m.contains("invalid id")));
    assert!(!wait_child(&mut child).success());
}

/// Kills a detached supervisor (and its child's group) if a test fails midway.
struct KillOnDrop(SupervisorInfo);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        kill_group(self.0.child_pid);
        kill_group(self.0.pid);
    }
}

#[tokio::test]
async fn detached_supervisor_survives_its_parent() {
    let state = tempfile::tempdir().unwrap();
    let spec = shell_spec(state.path(), "cat");
    let paths = paths(state.path(), "s1");
    let mut launcher = base_command(state.path());
    supervise_args(&mut launcher, &paths, "s1", &spec);
    // The launcher runs under a shell that we then kill with its whole process
    // group, as when an SSH session goes away.
    let mut parent = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#""$@"; exec sleep 600"#)
        .arg("sh")
        .arg(launcher.get_program())
        .args(launcher.get_args())
        .envs(launcher.get_envs().filter_map(|(k, v)| Some((k, v?))))
        .process_group(0)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(parent.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let info: SupervisorInfo =
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"));
    let cleanup = KillOnDrop(info.clone());
    assert_eq!(read_info(&paths.dir), Some(info.clone()));

    // Its own session, not a child of the launcher's shell.
    let (ppid, sid) = proc_ids(info.pid).unwrap();
    assert_eq!(sid, info.pid);
    assert_ne!(ppid, parent.id());

    kill_group(parent.id());
    parent.wait().unwrap();
    signal(info.pid, Signal::HUP);

    let client = Client::connect(&info.socket, true).await.unwrap();
    let status = client.status().await.unwrap();
    assert!(status.alive);
    assert_eq!(status.pid, info.pid);
    let mut stream = client.read(0).await.unwrap();
    client.write("still here").await.unwrap();
    records_until(&mut stream, is_out("still here")).await;
    client.terminate(None).await.unwrap();
    records_until(&mut stream, is_exit).await;
    drop((client, stream));
    wait_until("socket removed", || !info.socket.exists());
    wait_until("supervisor exited", || is_dead(info.pid));
    assert_eq!(read_info(&paths.dir), None);
    drop(cleanup);
}

#[tokio::test]
async fn launch_api_starts_a_detached_supervisor() {
    let state = tempfile::tempdir().unwrap();
    // A plain argv, so the inherited SHELL doesn't matter.
    let mut spec = shell_spec(state.path(), "unused");
    spec.exec = Exec::Argv(
        ["/bin/sh", "-c", "echo hi; exec cat"]
            .map(str::to_owned)
            .into(),
    );
    let request = LaunchRequest {
        id: "j1".into(),
        paths: paths(state.path(), "j1"),
        spec,
    };
    let wrapper = std::path::Path::new(BIN);

    let info = launch(wrapper, &request).await.unwrap();
    let cleanup = KillOnDrop(info.clone());
    assert_eq!(info.id, "j1");
    assert_eq!(read_info(&request.paths.dir), Some(info.clone()));
    let (ppid, sid) = proc_ids(info.pid).unwrap();
    assert_eq!(sid, info.pid);
    assert_ne!(ppid, std::process::id());

    let client = Client::connect(&info.socket, true).await.unwrap();
    let mut stream = client.read(0).await.unwrap();
    records_until(&mut stream, is_out("hi")).await;
    client.terminate(None).await.unwrap();
    records_until(&mut stream, is_exit).await;
    drop((client, stream));
    wait_until("supervisor exited", || is_dead(info.pid));
    drop(cleanup);

    // Failures come back as errors, not hangs.
    let mut bad = request.clone();
    bad.spec.cwd = "relative".into();
    let err = launch(wrapper, &bad).await.unwrap_err();
    assert!(err.to_string().contains("not absolute"), "{err}");
}
