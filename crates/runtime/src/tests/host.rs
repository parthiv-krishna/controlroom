//! An in-memory [`Host`]: supervisors are transcripts in memory with a [`FakeAgent`] behind the
//! stdin pipe. It outlives runtimes, so a test can "kill the daemon" (shut the runtime down)
//! and open a new one on the same store and transcripts.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use controlroom_drivers::fake::wire::{Command, Input};
use controlroom_drivers::fake::{AgentOutput, FakeAgent, FakeScript};
use controlroom_drivers::{Capabilities, Driver, FakeDriver};
use controlroom_protocol::ids::{RemotePath, Seq, SessionId, UnixMillis};
use controlroom_protocol::session::DriverKind;
use controlroom_protocol::{ErrorData, Resource, RpcError};
use controlroom_supervisor::{Exec, SpawnSpec};
use controlroom_transcript::{Body, ExitInfo, Line, Meta, Record, StopCause};
use futures::future::BoxFuture;
use tokio::sync::{oneshot, watch};

use crate::ports::{AgentLaunch, Agents, Clock, Host, HostError, RecordStream, SupervisorConn};

/// A clock that only moves when the test says so.
#[derive(Debug, Default)]
pub(crate) struct ManualClock(Mutex<i64>);

impl ManualClock {
    pub(crate) fn advance(&self, ms: i64) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) += ms;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> UnixMillis {
        UnixMillis(1_700_000_000_000 + *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// The fake agents config: `fake` is the only agent.
pub(crate) struct FakeAgents {
    pub capabilities: Capabilities,
}

impl Agents for FakeAgents {
    fn resolve(&self, agent: &str) -> Result<DriverKind, RpcError> {
        if agent == "fake" {
            return Ok(DriverKind::Fake);
        }
        Err(RpcError::new(
            ErrorData::NotFound {
                resource: Resource::Agent,
                id: agent.into(),
            },
            "unknown agent",
        ))
    }

    fn driver(&self, _kind: DriverKind) -> Box<dyn Driver> {
        Box::new(FakeDriver::new(self.capabilities))
    }

    fn spawn_spec(&self, launch: &AgentLaunch) -> Result<SpawnSpec, RpcError> {
        let mut argv = vec!["fake-agent".to_owned()];
        argv.extend(launch.args.iter().cloned());
        Ok(SpawnSpec {
            kind: controlroom_transcript::ProcessKind::Agent,
            exec: Exec::Argv(argv),
            cwd: launch.cwd.as_str().into(),
            env_file: None,
            stdin: controlroom_transcript::StdinMode::Pipe,
            timeout_ms: None,
            max_line_bytes: None,
            labels: Default::default(),
        })
    }
}

type Held = (String, oneshot::Sender<Result<Seq, HostError>>);

/// One session's supervised agent.
#[derive(Default)]
struct Proc {
    transcript: Vec<Record>,
    agent: Option<FakeAgent>,
    /// The supervisor runs (it exits after the agent).
    supervisor: bool,
    writer: bool,
    /// Writes are accepted but not logged until released (a supervisor busy with fsyncs).
    hold: bool,
    held: Vec<Held>,
}

struct State {
    procs: HashMap<SessionId, Proc>,
    files: HashMap<String, String>,
    removed: Vec<SessionId>,
    script: FakeScript,
    clock: Arc<ManualClock>,
}

/// The in-memory host.
#[derive(Clone)]
pub(crate) struct FakeHost {
    state: Arc<Mutex<State>>,
    changed: Arc<watch::Sender<u64>>,
}

impl FakeHost {
    pub(crate) fn new(script: FakeScript, clock: Arc<ManualClock>) -> Self {
        FakeHost {
            state: Arc::new(Mutex::new(State {
                procs: HashMap::new(),
                files: HashMap::new(),
                removed: Vec::new(),
                script,
                clock,
            })),
            changed: Arc::new(watch::channel(0).0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }

    /// Lets the session's agent past a gate.
    pub(crate) fn release(&self, id: SessionId, gate: &str) {
        {
            let mut state = self.lock();
            let now = state.clock.now();
            let proc = state.procs.get_mut(&id).expect("no such session");
            let out = proc
                .agent
                .as_mut()
                .map(|a| a.release(gate))
                .unwrap_or_default();
            proc.output(out, now);
        }
        self.bump();
    }

    /// SIGKILLs the supervisor: no exit record, streams end.
    pub(crate) fn kill_supervisor(&self, id: SessionId) {
        {
            let mut state = self.lock();
            let proc = state.procs.get_mut(&id).expect("no such session");
            proc.agent = None;
            proc.supervisor = false;
        }
        self.bump();
    }

    /// The agent dies (exit record), as on a crash while idle.
    pub(crate) fn kill_agent(&self, id: SessionId) {
        {
            let mut state = self.lock();
            let now = state.clock.now();
            let proc = state.procs.get_mut(&id).expect("no such session");
            proc.exit(now, None, Some(9), None);
        }
        self.bump();
    }

    /// Holds (or stops holding) writes before they are logged.
    pub(crate) fn hold_writes(&self, id: SessionId, hold: bool) {
        self.lock().procs.entry(id).or_default().hold = hold;
    }

    /// Logs the held writes, in order.
    pub(crate) fn release_writes(&self, id: SessionId) {
        {
            let mut state = self.lock();
            let now = state.clock.now();
            let proc = state.procs.get_mut(&id).expect("no such session");
            proc.hold = false;
            for (line, reply) in std::mem::take(&mut proc.held) {
                let _ = reply.send(proc.log_and_feed(line, now));
            }
        }
        self.bump();
    }

    /// Number of writes waiting to be logged.
    pub(crate) fn held_writes(&self, id: SessionId) -> usize {
        self.lock().procs.get(&id).map_or(0, |p| p.held.len())
    }

    /// Fails the held writes without logging them.
    pub(crate) fn drop_writes(&self, id: SessionId) {
        {
            let mut state = self.lock();
            let proc = state.procs.get_mut(&id).expect("no such session");
            proc.hold = false;
            for (_, reply) in std::mem::take(&mut proc.held) {
                let _ = reply.send(Err(HostError("dropped".into())));
            }
        }
        self.bump();
    }

    /// The commands written to the agent, in order.
    pub(crate) fn commands(&self, id: SessionId) -> Vec<Command> {
        self.lock()
            .procs
            .get(&id)
            .map(|p| {
                p.transcript
                    .iter()
                    .filter_map(|r| match &r.body {
                        Body::In(line) => serde_json::from_str::<Input>(&line.raw).ok(),
                        _ => None,
                    })
                    .map(|input| input.command)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The prompt texts written, in order.
    pub(crate) fn prompts(&self, id: SessionId) -> Vec<String> {
        self.commands(id)
            .into_iter()
            .filter_map(|c| match c {
                Command::Prompt { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn transcript(&self, id: SessionId) -> Vec<Record> {
        self.lock()
            .procs
            .get(&id)
            .map(|p| p.transcript.clone())
            .unwrap_or_default()
    }

    pub(crate) fn file(&self, path: &str) -> Option<String> {
        self.lock().files.get(path).cloned()
    }

    pub(crate) fn removed(&self, id: SessionId) -> bool {
        self.lock().removed.contains(&id)
    }

    pub(crate) fn supervisor_running(&self, id: SessionId) -> bool {
        self.lock().procs.get(&id).is_some_and(|p| p.supervisor)
    }

    /// Waits until `pred` holds for the host state (a gate on the host's changes, not a sleep).
    pub(crate) async fn wait(&self, what: &str, pred: impl Fn(&FakeHost) -> bool) {
        let mut changed = self.changed.subscribe();
        let wait = async {
            while !pred(self) {
                if changed.changed().await.is_err() {
                    return;
                }
            }
        };
        if tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .is_err()
        {
            panic!("timed out waiting for {what}");
        }
    }
}

impl Proc {
    fn append(&mut self, ts: UnixMillis, body: Body) -> Seq {
        let seq = self.transcript.last().map_or(1, |r| r.seq + 1);
        self.transcript.push(Record {
            seq,
            ts: u64::try_from(ts.0).unwrap_or(0),
            body,
        });
        Seq(seq)
    }

    fn log_and_feed(&mut self, line: String, now: UnixMillis) -> Result<Seq, HostError> {
        let Some(agent) = self.agent.as_mut() else {
            return Err(HostError("child_exited".into()));
        };
        let out = agent.input(&line);
        let seq = self.append(now, Body::In(Line::new(line)));
        self.output(out, now);
        Ok(seq)
    }

    fn output(&mut self, out: AgentOutput, now: UnixMillis) {
        for line in out.lines {
            self.append(now, Body::Out(Line::new(line)));
        }
        if let Some(code) = out.exit {
            self.exit(now, Some(code), None, None);
        }
    }

    fn exit(
        &mut self,
        now: UnixMillis,
        code: Option<i32>,
        signal: Option<i32>,
        cause: Option<StopCause>,
    ) {
        if self.agent.take().is_none() {
            return;
        }
        self.append(
            now,
            Body::Exit(ExitInfo {
                code,
                signal,
                cause,
            }),
        );
        self.supervisor = false;
    }
}

impl Host for FakeHost {
    fn resolve_cwd(&self, cwd: &RemotePath) -> BoxFuture<'_, Result<RemotePath, RpcError>> {
        let cwd = match cwd.as_str().strip_prefix('~') {
            Some(rest) => RemotePath::new(format!("/home/alice{rest}")),
            None => cwd.clone(),
        };
        Box::pin(async move { Ok(cwd) })
    }

    fn launch(&self, session: SessionId, spec: SpawnSpec) -> BoxFuture<'_, Result<(), HostError>> {
        Box::pin(async move {
            {
                let mut state = self.lock();
                let now = state.clock.now();
                let script = state.script.clone();
                let proc = state.procs.entry(session).or_default();
                if proc.supervisor {
                    return Err(HostError("a supervisor is already running".into()));
                }
                let Exec::Argv(argv) = &spec.exec else {
                    return Err(HostError("expected an argv".into()));
                };
                proc.agent = Some(FakeAgent::launch(script, &argv[1..]));
                proc.supervisor = true;
                proc.append(
                    now,
                    Body::Meta(Box::new(Meta {
                        id: session.to_string(),
                        argv: argv.clone(),
                        ..Meta::default()
                    })),
                );
            }
            self.bump();
            Ok(())
        })
    }

    fn connect(
        &self,
        session: SessionId,
    ) -> BoxFuture<'_, Result<Option<Arc<dyn SupervisorConn>>, HostError>> {
        Box::pin(async move {
            let mut state = self.lock();
            let Some(proc) = state.procs.get_mut(&session).filter(|p| p.supervisor) else {
                return Ok(None);
            };
            if proc.writer {
                return Err(HostError("writer_busy".into()));
            }
            proc.writer = true;
            let conn: Arc<dyn SupervisorConn> = Arc::new(FakeConn {
                host: self.clone(),
                session,
            });
            Ok(Some(conn))
        })
    }

    fn read_transcript(
        &self,
        session: SessionId,
        from: Seq,
    ) -> BoxFuture<'_, Result<Box<dyn RecordStream>, HostError>> {
        let records: Vec<Record> = self
            .transcript(session)
            .into_iter()
            .filter(|r| r.seq >= from.0)
            .collect();
        Box::pin(async move {
            let stream: Box<dyn RecordStream> = Box::new(Snapshot(records.into_iter()));
            Ok(stream)
        })
    }

    fn write_file(
        &self,
        session: SessionId,
        name: String,
        contents: String,
    ) -> BoxFuture<'_, Result<RemotePath, HostError>> {
        let path = format!("/home/alice/.local/state/controlroom/sessions/{session}/{name}");
        self.lock().files.insert(path.clone(), contents);
        Box::pin(async move { Ok(RemotePath::new(path)) })
    }

    fn remove(&self, session: SessionId) -> BoxFuture<'_, Result<(), HostError>> {
        {
            let mut state = self.lock();
            state.procs.remove(&session);
            state.removed.push(session);
        }
        Box::pin(async { Ok(()) })
    }
}

struct Snapshot(std::vec::IntoIter<Record>);

impl RecordStream for Snapshot {
    fn next(&mut self) -> BoxFuture<'_, Option<Record>> {
        let next = self.0.next();
        Box::pin(async move { next })
    }
}

/// The writer connection; dropping it releases the writer role.
struct FakeConn {
    host: FakeHost,
    session: SessionId,
}

impl Drop for FakeConn {
    fn drop(&mut self) {
        if let Some(proc) = self.host.lock().procs.get_mut(&self.session) {
            proc.writer = false;
        }
    }
}

impl SupervisorConn for FakeConn {
    fn write(&self, line: String) -> BoxFuture<'_, Result<Seq, HostError>> {
        Box::pin(async move {
            let reply = {
                let mut state = self.host.lock();
                let now = state.clock.now();
                let proc = state
                    .procs
                    .get_mut(&self.session)
                    .ok_or_else(|| HostError("gone".into()))?;
                if !proc.supervisor {
                    return Err(HostError("the supervisor is gone".into()));
                }
                if !proc.hold {
                    let result = proc.log_and_feed(line, now);
                    drop(state);
                    self.host.bump();
                    return result;
                }
                let (tx, rx) = oneshot::channel();
                proc.held.push((line, tx));
                rx
            };
            self.host.bump();
            reply
                .await
                .unwrap_or_else(|_| Err(HostError("dropped".into())))
        })
    }

    fn barrier(&self) -> BoxFuture<'_, Result<Seq, HostError>> {
        Box::pin(async move {
            let mut changed = self.host.changed.subscribe();
            loop {
                {
                    let state = self.host.lock();
                    let proc = state.procs.get(&self.session);
                    match proc {
                        Some(p) if p.held.is_empty() => {
                            return Ok(Seq(p.transcript.last().map_or(0, |r| r.seq)));
                        }
                        Some(p) if p.supervisor => {}
                        _ => return Err(HostError("the supervisor is gone".into())),
                    }
                }
                if changed.changed().await.is_err() {
                    return Err(HostError("host dropped".into()));
                }
            }
        })
    }

    fn read(&self, from: Seq) -> BoxFuture<'_, Result<Box<dyn RecordStream>, HostError>> {
        let stream: Box<dyn RecordStream> = Box::new(Follow {
            host: self.host.clone(),
            session: self.session,
            next: from.0,
            changed: self.host.changed.subscribe(),
        });
        Box::pin(async move { Ok(stream) })
    }

    fn terminate(&self, _grace: Duration) -> BoxFuture<'_, Result<(), HostError>> {
        Box::pin(async move {
            {
                let mut state = self.host.lock();
                let now = state.clock.now();
                if let Some(proc) = state.procs.get_mut(&self.session) {
                    proc.exit(now, None, Some(15), Some(StopCause::Terminate));
                }
            }
            self.host.bump();
            Ok(())
        })
    }
}

/// A live read stream: records from `next`, ending once the supervisor is gone.
struct Follow {
    host: FakeHost,
    session: SessionId,
    next: u64,
    changed: watch::Receiver<u64>,
}

impl RecordStream for Follow {
    fn next(&mut self) -> BoxFuture<'_, Option<Record>> {
        Box::pin(async move {
            loop {
                {
                    let state = self.host.lock();
                    let proc = state.procs.get(&self.session)?;
                    if let Some(record) = proc.transcript.iter().find(|r| r.seq >= self.next) {
                        self.next = record.seq + 1;
                        return Some(record.clone());
                    }
                    if !proc.supervisor {
                        return None;
                    }
                }
                self.changed.changed().await.ok()?;
            }
        })
    }
}
