//! An in-memory [`JobHost`]: job supervisors are transcripts in memory that the test writes
//! output and exits into. Like [`super::FakeHost`] it outlives runtimes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use controlroom_protocol::ids::{JobId, Seq};
use controlroom_protocol::{ErrorData, RpcError};
use controlroom_supervisor::{Exec, SpawnSpec};
use controlroom_transcript::{
    Body, ExitInfo, Line, Meta, ProcessKind, Record, StdinMode, StopCause,
};
use futures::future::BoxFuture;
use tokio::sync::watch;

use super::host::ManualClock;
use crate::ports::{Clock, HostError, JobHost, JobLaunch, JobRef, RecordStream, SupervisorConn};

#[derive(Default)]
struct JobProc {
    transcript: Vec<Record>,
    running: bool,
}

#[derive(Default)]
struct State {
    procs: HashMap<JobId, JobProc>,
    launches: Vec<JobLaunch>,
    fail_spec: Option<String>,
}

/// The in-memory job host.
#[derive(Clone)]
pub(crate) struct FakeJobHost {
    state: Arc<Mutex<State>>,
    changed: Arc<watch::Sender<u64>>,
    clock: Arc<ManualClock>,
}

impl FakeJobHost {
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        FakeJobHost {
            state: Arc::default(),
            changed: Arc::new(watch::channel(0).0),
            clock,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }

    fn append(&self, job: JobId, body: Body) {
        let now = self.clock.now();
        {
            let mut state = self.lock();
            let proc = state.procs.get_mut(&job).expect("no such job");
            assert!(proc.running, "job {job} is not running");
            let seq = proc.transcript.last().map_or(1, |r| r.seq + 1);
            if matches!(body, Body::Exit(_)) {
                proc.running = false;
            }
            proc.transcript.push(Record {
                seq,
                ts: u64::try_from(now.0).unwrap_or(0),
                body,
            });
        }
        self.bump();
    }

    /// The job prints `lines` (one record each).
    pub(crate) fn output(&self, job: JobId, lines: &[&str]) {
        for line in lines {
            self.append(job, Body::Out(Line::new(*line)));
        }
    }

    /// The job exits by itself.
    pub(crate) fn exit(&self, job: JobId, code: i32) {
        self.stop(job, Some(code), None, None);
    }

    /// The supervisor stops the job (its timeout, or a terminate).
    pub(crate) fn stop(
        &self,
        job: JobId,
        code: Option<i32>,
        signal: Option<i32>,
        cause: Option<StopCause>,
    ) {
        self.append(
            job,
            Body::Exit(ExitInfo {
                code,
                signal,
                cause,
            }),
        );
    }

    /// SIGKILLs the job's supervisor (a reboot): no exit record.
    pub(crate) fn kill_supervisor(&self, job: JobId) {
        if let Some(proc) = self.lock().procs.get_mut(&job) {
            proc.running = false;
        }
        self.bump();
    }

    /// Makes the next spawn specs fail with `error`.
    pub(crate) fn fail_spec(&self, error: Option<&str>) {
        self.lock().fail_spec = error.map(str::to_owned);
    }

    pub(crate) fn launches(&self) -> Vec<JobLaunch> {
        self.lock().launches.clone()
    }

    /// The seq of the job's last record.
    pub(crate) fn last_seq(&self, job: JobId) -> u64 {
        self.lock()
            .procs
            .get(&job)
            .and_then(|p| p.transcript.last())
            .map_or(0, |r| r.seq)
    }

    pub(crate) fn running(&self, job: JobId) -> bool {
        self.lock().procs.get(&job).is_some_and(|p| p.running)
    }

    /// Waits until `pred` holds (a gate on the host's changes, not a sleep).
    pub(crate) async fn wait(&self, what: &str, pred: impl Fn(&FakeJobHost) -> bool) {
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

impl JobHost for FakeJobHost {
    fn spawn_spec(&self, launch: &JobLaunch) -> Result<SpawnSpec, RpcError> {
        let mut state = self.lock();
        if let Some(error) = &state.fail_spec {
            return Err(RpcError::new(
                ErrorData::SandboxUnavailable {
                    detail: error.clone(),
                },
                error.clone(),
            ));
        }
        state.launches.push(launch.clone());
        Ok(SpawnSpec {
            kind: ProcessKind::Job,
            exec: Exec::Shell(launch.command.clone()),
            cwd: launch.cwd.as_str().into(),
            env_file: None,
            stdin: StdinMode::Null,
            timeout_ms: launch.timeout_ms,
            max_line_bytes: None,
            labels: Default::default(),
            plain_log: Some(launch.log_path.as_str().into()),
        })
    }

    fn launch(&self, job: JobRef, spec: SpawnSpec) -> BoxFuture<'_, Result<(), HostError>> {
        Box::pin(async move {
            let now = self.clock.now();
            {
                let mut state = self.lock();
                let proc = state.procs.entry(job.job).or_default();
                proc.running = true;
                proc.transcript.push(Record {
                    seq: 1,
                    ts: u64::try_from(now.0).unwrap_or(0),
                    body: Body::Meta(Box::new(Meta {
                        id: job.job.to_string(),
                        kind: spec.kind,
                        argv: spec.argv(Some("/bin/sh")),
                        ..Meta::default()
                    })),
                });
            }
            self.bump();
            Ok(())
        })
    }

    fn connect(
        &self,
        job: JobRef,
    ) -> BoxFuture<'_, Result<Option<Arc<dyn SupervisorConn>>, HostError>> {
        let running = self.running(job.job);
        let conn: Option<Arc<dyn SupervisorConn>> = running.then(|| {
            Arc::new(FakeJobConn {
                host: self.clone(),
                job: job.job,
            }) as Arc<dyn SupervisorConn>
        });
        Box::pin(async move { Ok(conn) })
    }

    fn read_transcript(
        &self,
        job: JobRef,
        from: Seq,
    ) -> BoxFuture<'_, Result<Box<dyn RecordStream>, HostError>> {
        let records: Vec<Record> = self
            .lock()
            .procs
            .get(&job.job)
            .map(|p| {
                p.transcript
                    .iter()
                    .filter(|r| r.seq >= from.0)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        Box::pin(
            async move { Ok(Box::new(Snapshot(records.into_iter())) as Box<dyn RecordStream>) },
        )
    }
}

struct Snapshot(std::vec::IntoIter<Record>);

impl RecordStream for Snapshot {
    fn next(&mut self) -> BoxFuture<'_, Option<Record>> {
        let next = self.0.next();
        Box::pin(async move { next })
    }
}

struct FakeJobConn {
    host: FakeJobHost,
    job: JobId,
}

impl SupervisorConn for FakeJobConn {
    fn write(&self, _line: String) -> BoxFuture<'_, Result<Seq, HostError>> {
        Box::pin(async { Err(HostError("no_stdin".into())) })
    }

    fn barrier(&self) -> BoxFuture<'_, Result<Seq, HostError>> {
        Box::pin(async { Err(HostError("not used for jobs".into())) })
    }

    fn read(&self, from: Seq) -> BoxFuture<'_, Result<Box<dyn RecordStream>, HostError>> {
        let stream: Box<dyn RecordStream> = Box::new(Follow {
            host: self.host.clone(),
            job: self.job,
            next: from.0,
            changed: self.host.changed.subscribe(),
        });
        Box::pin(async move { Ok(stream) })
    }

    fn terminate(&self, _grace: Duration) -> BoxFuture<'_, Result<(), HostError>> {
        Box::pin(async move {
            if self.host.running(self.job) {
                self.host
                    .stop(self.job, None, Some(15), Some(StopCause::Terminate));
            }
            Ok(())
        })
    }
}

/// A live read stream, ending after the exit record or when the supervisor is gone.
struct Follow {
    host: FakeJobHost,
    job: JobId,
    next: u64,
    changed: watch::Receiver<u64>,
}

impl RecordStream for Follow {
    fn next(&mut self) -> BoxFuture<'_, Option<Record>> {
        Box::pin(async move {
            loop {
                {
                    let state = self.host.lock();
                    let proc = state.procs.get(&self.job)?;
                    if let Some(record) = proc.transcript.iter().find(|r| r.seq >= self.next) {
                        self.next = record.seq + 1;
                        return Some(record.clone());
                    }
                    if !proc.running {
                        return None;
                    }
                }
                self.changed.changed().await.ok()?;
            }
        })
    }
}
