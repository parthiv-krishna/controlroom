//! Running a real `pi --mode rpc` process and recording every line.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use controlroom_transcript::{Body, ExitInfo, Line, Meta, Record, Writer};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::watch;

use crate::env::PiEnv;
use crate::fixture::{Action, Fixture, read_transcript};

/// How long a single wait may take before the scenario fails.
pub const WAIT: Duration = Duration::from_secs(30);

/// The recording state shared by the stdio pumps.
struct Log {
    writer: Writer,
    /// Every record appended, for waits.
    records: Vec<Record>,
    actions: Vec<Action>,
}

/// Appends records through a transcript [`Writer`] and wakes waiters.
#[derive(Clone)]
struct Recorder {
    log: Arc<Mutex<Log>>,
    changed: watch::Sender<usize>,
}

impl Recorder {
    fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let (writer, _) =
            Writer::open(path).with_context(|| format!("opening {}", path.display()))?;
        Ok(Self {
            log: Arc::new(Mutex::new(Log {
                writer,
                records: Vec::new(),
                actions: Vec::new(),
            })),
            changed: watch::Sender::new(0),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Log> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Append a record and wake waiters. A failed append is reported but does
    /// not stop the recording; the transcript check at the end catches it.
    fn push(&self, body: Body) {
        let len = {
            let mut log = self.lock();
            match log.writer.append(body) {
                Ok(record) => log.records.push(record),
                Err(err) => eprintln!("fixture-tool: appending a record failed: {err}"),
            }
            log.records.len()
        };
        self.changed.send_replace(len);
    }

    /// Note a recorder action after the last record.
    fn action(&self, make: impl FnOnce(u64) -> Action) {
        let mut log = self.lock();
        let after = log.writer.last_seq();
        log.actions.push(make(after));
    }

    fn records(&self) -> Vec<Record> {
        self.lock().records.clone()
    }

    fn actions(&self) -> Vec<Action> {
        self.lock().actions.clone()
    }

    /// Wait until a record at index `>= from` satisfies `pred`; returns its index.
    async fn wait_from(&self, from: usize, pred: impl Fn(&Record) -> bool) -> Option<usize> {
        let mut rx = self.changed.subscribe();
        loop {
            let found = self
                .lock()
                .records
                .iter()
                .enumerate()
                .skip(from)
                .find(|(_, r)| pred(r))
                .map(|(i, _)| i);
            if found.is_some() {
                return found;
            }
            rx.changed().await.ok()?;
        }
    }
}

/// A running pi process. Dropping it SIGKILLs the process group.
pub struct PiProcess {
    child: Child,
    pid: u32,
    stdin: Option<ChildStdin>,
    recorder: Recorder,
    transcript: PathBuf,
    /// Records before this index have been consumed by `wait_*`.
    cursor: usize,
    pumps: Vec<tokio::task::JoinHandle<()>>,
    exited: bool,
}

impl PiProcess {
    /// Spawn `pi --mode rpc <args>` in `env`, recording to `transcript`.
    /// `meta` gets the argv, cwd and pids and becomes the first record.
    pub fn spawn(env: &PiEnv, args: &[String], transcript: &Path, mut meta: Meta) -> Result<Self> {
        let recorder = Recorder::open(transcript)?;
        let pi = std::env::var("CONTROLROOM_PI").unwrap_or_else(|_| "pi".into());
        let mut argv = vec![pi.clone(), "--mode".into(), "rpc".into()];
        argv.extend(args.iter().cloned());
        let mut cmd = Command::new(&pi);
        cmd.args(&argv[1..])
            .current_dir(env.work_dir())
            .env_clear()
            .envs(env.vars())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let mut child = cmd.spawn().with_context(|| format!("spawning {pi}"))?;
        let pid = child.id().ok_or_else(|| anyhow!("pi exited immediately"))?;
        meta.argv = argv;
        meta.cwd = env.work_dir().display().to_string();
        meta.child_pid = pid;
        meta.supervisor_pid = std::process::id();
        recorder.push(Body::Meta(Box::new(meta)));
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;
        let pumps = vec![
            tokio::spawn(pump(stdout, Body::Out, recorder.clone())),
            tokio::spawn(pump(stderr, Body::Stderr, recorder.clone())),
        ];
        Ok(Self {
            stdin: child.stdin.take(),
            child,
            pid,
            recorder,
            transcript: transcript.to_owned(),
            cursor: 0,
            pumps,
            exited: false,
        })
    }

    /// Record and write one stdin line.
    pub async fn send_raw(&mut self, line: &str) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| anyhow!("stdin closed"))?;
        self.recorder.push(Body::In(Line::new(line)));
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Send a command object.
    pub async fn send(&mut self, command: Value) -> Result<()> {
        self.send_raw(&command.to_string()).await
    }

    /// Note that the mock model released `gate` (never sent to pi).
    pub fn released_gate(&self, gate: &str) {
        self.recorder.action(|after_seq| Action::ReleaseGate {
            after_seq,
            gate: gate.to_owned(),
        });
    }

    /// Wait for the next stdout record (after earlier waits) matching `pred`
    /// and return its JSON.
    pub async fn wait_out(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Result<Value> {
        let matches = |r: &Record| {
            matches!(&r.body, Body::Out(line)
                if serde_json::from_str::<Value>(&line.raw).ok().is_some_and(|v| pred(&v)))
        };
        let found = tokio::time::timeout(WAIT, self.recorder.wait_from(self.cursor, matches))
            .await
            .ok()
            .flatten();
        let Some(index) = found else {
            bail!("timed out waiting for {what}\n{}", self.tail());
        };
        self.cursor = index + 1;
        let records = self.recorder.records();
        Ok(serde_json::from_str(
            records[index].raw().unwrap_or_default(),
        )?)
    }

    /// Wait for the next event of `event_type`.
    pub async fn wait_event(&mut self, event_type: &str) -> Result<Value> {
        self.wait_out(event_type, |v| v["type"] == event_type).await
    }

    /// Wait for the response to the command with this `id`.
    pub async fn wait_response(&mut self, id: &str) -> Result<Value> {
        self.wait_out(&format!("response {id}"), |v| {
            v["type"] == "response" && v["id"] == id
        })
        .await
    }

    /// Send a command with `id` and wait for its response.
    pub async fn call(&mut self, id: &str, mut command: Value) -> Result<Value> {
        command["id"] = Value::String(id.to_owned());
        self.send(command).await?;
        self.wait_response(id).await
    }

    /// Close stdin (pi's orderly shutdown) and wait for the exit.
    pub async fn close(&mut self) -> Result<()> {
        self.recorder
            .action(|after_seq| Action::CloseStdin { after_seq });
        self.stdin = None;
        self.wait_exit().await
    }

    /// Send a signal to pi itself (not its group).
    pub fn signal(&self, signal: &str) -> Result<()> {
        self.recorder.action(|after_seq| Action::Signal {
            after_seq,
            signal: signal.to_owned(),
        });
        kill(signal, &self.pid.to_string())
    }

    /// Wait for pi to exit and record the exit.
    pub async fn wait_exit(&mut self) -> Result<()> {
        let status = match tokio::time::timeout(WAIT, self.child.wait()).await {
            Ok(status) => status?,
            Err(_) => bail!("timed out waiting for pi to exit\n{}", self.tail()),
        };
        // Let the pumps drain what pi wrote before exiting. Grandchildren
        // could hold the pipes open, so the group is killed first.
        let _ = kill("KILL", &format!("-{}", self.pid));
        for pump in self.pumps.drain(..) {
            let _ = tokio::time::timeout(Duration::from_secs(5), pump).await;
        }
        use std::os::unix::process::ExitStatusExt;
        self.recorder.push(Body::Exit(ExitInfo {
            code: status.code(),
            signal: status.signal(),
            cause: None,
        }));
        self.exited = true;
        Ok(())
    }

    /// The finished recording, read back from the transcript file, so it is
    /// exactly what was written.
    pub fn fixture(&self) -> Result<Fixture> {
        let records = read_transcript(&self.transcript)?;
        if records != self.recorder.records() {
            bail!(
                "{}: the file differs from the records appended",
                self.transcript.display()
            );
        }
        Ok(Fixture {
            records,
            actions: self.recorder.actions(),
        })
    }

    /// The last few records, for error messages.
    fn tail(&self) -> String {
        let records = self.recorder.records();
        let start = records.len().saturating_sub(15);
        records[start..]
            .iter()
            .map(|r| {
                let raw: String = serde_json::to_string(r)
                    .unwrap_or_default()
                    .chars()
                    .take(300)
                    .collect();
                format!("  {raw}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for PiProcess {
    fn drop(&mut self) {
        if !self.exited {
            let _ = kill("KILL", &format!("-{}", self.pid));
        }
    }
}

/// Record every LF-terminated line of `reader` as `body(line)`. Lines are
/// split on LF only (never on U+2028/U+2029), and a trailing CR is stripped.
async fn pump(reader: impl AsyncRead + Unpin, body: fn(Line) -> Body, recorder: Recorder) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                if buf.last() == Some(&b'\n') {
                    buf.pop();
                }
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
                let lossy = std::str::from_utf8(&buf).is_err();
                let raw = String::from_utf8_lossy(&buf).into_owned();
                recorder.push(body(Line {
                    raw,
                    truncated: 0,
                    lossy,
                }));
            }
        }
    }
}

/// Run `kill -<signal> -- <target>`, keeping this crate free of `unsafe`.
fn kill(signal: &str, target: &str) -> Result<()> {
    let status = std::process::Command::new("kill")
        .args([&format!("-{signal}"), "--", target])
        .stderr(Stdio::null())
        .status()
        .context("running kill")?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("kill -{signal} {target} failed: {status}"))
    }
}

/// The absolute session file pi reported in a `get_state` response.
pub fn session_file(state: &Value) -> Result<PathBuf> {
    state["data"]["sessionFile"]
        .as_str()
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| anyhow!("get_state has no sessionFile: {state}"))
}

/// The path of `path` as a string for a command.
pub fn path_str(path: &Path) -> String {
    path.display().to_string()
}
