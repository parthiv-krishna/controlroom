//! `controlroomd connect` and the daemon instance end to end: starting a detached daemon on
//! demand, bridging, outliving the connection, one daemon per user, restart after SIGTERM.
//! Every daemon a test starts is killed when the test ends, pass or fail.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, getsid, kill_process};
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_controlroomd");
const DEADLINE: Duration = Duration::from_secs(20);

/// An isolated `HOME`; kills its daemons on drop.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        Self {
            dir: tempfile::Builder::new().prefix("crd-").tempdir().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn state(&self) -> PathBuf {
        self.path().join(".local/state/controlroom")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .env("HOME", self.path())
            .env("NONO_ALLOW", "/")
            .stdin(Stdio::null());
        cmd
    }

    /// Pids of `serve` processes using this home's state dir.
    fn daemons(&self) -> Vec<(i32, Vec<String>)> {
        let state = self.state().display().to_string();
        let mut found = Vec::new();
        for entry in std::fs::read_dir("/proc").unwrap().flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            let Ok(raw) = std::fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            let argv: Vec<String> = raw
                .split(|b| *b == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            let zombie = std::fs::read_to_string(entry.path().join("stat"))
                .map(|s| s.rsplit(") ").next().unwrap_or("").starts_with('Z'))
                .unwrap_or(true);
            if !zombie && argv.get(1).map(String::as_str) == Some("serve") && argv.contains(&state)
            {
                found.push((pid, argv));
            }
        }
        found
    }

    fn wait_no_daemon(&self) {
        let deadline = Instant::now() + DEADLINE;
        while !self.daemons().is_empty() {
            assert!(Instant::now() < deadline, "the daemon did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        for (pid, _) in self.daemons() {
            if let Some(pid) = Pid::from_raw(pid) {
                let _ = kill_process(pid, Signal::KILL);
            }
        }
    }
}

/// A running `connect` bridge.
struct Bridge {
    child: Child,
    lines: BufReader<std::process::ChildStdout>,
}

impl Bridge {
    fn start(home: &Home) -> Self {
        let mut child = home
            .command(&["connect"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let lines = BufReader::new(child.stdout.take().unwrap());
        Self { child, lines }
    }

    /// Sends `hello` and returns the response.
    fn hello(&mut self) -> Value {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin
            .write_all(
                br#"{"jsonrpc":"2.0","id":7,"method":"hello","params":{"protocol_version":{"major":1,"minor":0},"client_name":"test","role":{"kind":"debug"}}}
"#,
            )
            .unwrap();
        let mut line = String::new();
        self.lines.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line:?}: {e}"))
    }

    /// Closes stdin and waits for the bridge to exit.
    fn close(mut self) -> std::process::Output {
        drop(self.child.stdin.take());
        self.child.wait_with_output().unwrap()
    }
}

#[test]
fn connect_starts_a_detached_daemon_that_outlives_the_connection() {
    let home = Home::new();
    let mut bridge = Bridge::start(&home);
    let response = bridge.hello();
    assert_eq!(response["id"], 7);
    assert!(response["result"]["daemon_id"].is_string(), "{response}");
    let out = bridge.close();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Disconnecting did nothing: the daemon still runs, detached, as `…/controlroomd serve`.
    let daemons = home.daemons();
    assert_eq!(daemons.len(), 1, "{daemons:?}");
    let (pid, argv) = &daemons[0];
    assert!(argv[0].ends_with("/controlroomd"), "{argv:?}");
    let sid = getsid(Pid::from_raw(*pid)).unwrap();
    assert_eq!(
        sid.as_raw_nonzero().get(),
        *pid,
        "the daemon leads its own session"
    );
    // Inherited NONO_* variables never reach the daemon (or what it starts).
    let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
    assert!(!String::from_utf8_lossy(&environ).contains("NONO_ALLOW"));
    let mode = std::fs::metadata(home.state().join("run/daemon.sock")).unwrap();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777,
        0o600
    );

    // A second connection reuses it; `serve --detach` sees it running.
    let mut again = Bridge::start(&home);
    assert_eq!(again.hello()["id"], 7);
    again.close();
    let detach = home.command(&["serve", "--detach"]).output().unwrap();
    assert!(detach.status.success());
    assert!(String::from_utf8_lossy(&detach.stderr).contains("already running"));
    assert_eq!(home.daemons().len(), 1);

    // SIGTERM ends it; the next connection starts a new one over the stale socket.
    kill_process(Pid::from_raw(*pid).unwrap(), Signal::TERM).unwrap();
    home.wait_no_daemon();
    let mut restarted = Bridge::start(&home);
    assert!(restarted.hello()["result"]["daemon_id"].is_string());
    restarted.close();
    let after = home.daemons();
    assert_eq!(after.len(), 1);
    assert_ne!(after[0].0, *pid);
}

#[test]
fn concurrent_connects_share_one_daemon() {
    let home = Home::new();
    let mut bridges: Vec<Bridge> = (0..4).map(|_| Bridge::start(&home)).collect();
    for bridge in &mut bridges {
        assert_eq!(bridge.hello()["id"], 7);
    }
    for bridge in bridges {
        let out = bridge.close();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(home.daemons().len(), 1);
}

#[test]
fn connect_fails_cleanly_without_home() {
    let out = Command::new(BIN)
        .arg("connect")
        .env_remove("HOME")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        out.stdout.is_empty(),
        "stdout must stay a clean JSON-RPC stream"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("controlroomd connect: HOME is not set"));
}
