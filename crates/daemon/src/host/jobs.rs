//! Background jobs on the host and their persisted event log (design §3.6, `docs/protocol.md`
//! §6 "Job event log").
//!
//! - A job runs in a detached job supervisor (`kind: job`, `stdin: null`, its plain log at
//!   `<root>/.controlroom/jobs/<id>.log`) under the workspace's profile, as `$SHELL -lc`.
//! - The host follows every running job's transcript and appends to the event log in
//!   `host.sqlite`, one revision sequence per host: `started` (before the process exists),
//!   `progress` (line count, plain-log offset, follow matches; batched), `follow`, `unfollow`,
//!   `killed`, `exit` (once the output is fully scanned). The job's current state (`jobs`
//!   table) changes in the same transaction as its event.
//! - The host never decides wakes and never waits for a subscriber. `jobs.subscribe {since}`
//!   returns the backlog and then streams `jobs.event`; appends and subscribes are serialized,
//!   so the backlog and the live events neither overlap nor leave a gap.
//! - A restarted daemon resumes scanning each running job where it stopped; a job whose
//!   supervisor is gone without an exit record ends `exit {lost}`.
//! - `jobs.ack {rev}` lets the host prune the events of jobs that ended at or before `rev`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use controlroom_protocol::daemon::FollowFrom;
use controlroom_protocol::host::{
    HostOp, HostResult, JobEvent, JobEventKind, JobFollowSpec, JobsAckParams, JobsEventParams,
    JobsSubscribeParams, JobsSubscribeResult, KilledBy, notifications,
};
use controlroom_protocol::ids::{CallId, JobId, Rev, SubscriptionId, UnixMillis, WorkspaceId};
use controlroom_protocol::session::{Job, JobFollow, JobStatus};
use controlroom_protocol::{ErrorData, RemotePath, Resource, RpcError};
use controlroom_supervisor::plain::plain_line;
use controlroom_supervisor::{Client, Exec, LaunchRequest, Paths, SpawnSpec, launch};
use controlroom_transcript::{Body, ExitInfo, ProcessKind, Record, StdinMode, StopCause};
use regex::Regex;
use rusqlite::{OptionalExtension, params};

use super::Host;
use super::fsops::{Context, invalid};
use super::shells::run::signal_name;
use super::store::{internal, meta_get, meta_set};
use super::workspaces::Workspace;
use crate::dirs::{create_private_dir, write_private_file};
use crate::shell::login_shell;

/// Default follow buffer.
const DEFAULT_BUFFER_SECONDS: u32 = 15;
/// How often a running job's new lines are logged as one `progress` event.
const PROGRESS_EVERY: Duration = Duration::from_secs(1);
/// Longest job output line kept whole.
const MAX_LINE_BYTES: u64 = 64 << 10;
/// Longest follow pattern and its compiled size.
const MAX_PATTERN: usize = 1024;
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// A job's live state.
struct State {
    job: Job,
    follow: Option<Regex>,
    scanned_seq: u64,
    offset: u64,
    killed: bool,
}

#[derive(Default)]
struct Inner {
    jobs: HashMap<JobId, State>,
    subscribers: HashMap<u64, SubscriptionId>,
}

/// The host's jobs.
#[derive(Default)]
pub struct Jobs {
    inner: Mutex<Inner>,
    /// Serializes appends to the event log with their broadcast, and subscribes.
    log: tokio::sync::Mutex<()>,
}

fn job_not_found(id: &JobId) -> RpcError {
    RpcError::new(
        ErrorData::NotFound {
            resource: Resource::Job,
            id: id.to_string(),
        },
        "no such job on this host",
    )
}

fn compile(pattern: &str) -> Result<Regex, RpcError> {
    if pattern.len() > MAX_PATTERN {
        return Err(invalid(format!(
            "the pattern is longer than {MAX_PATTERN} bytes"
        )));
    }
    regex::RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| invalid(format!("bad pattern: {e}")))
}

/// An event to append, with how it changes the job.
struct Change {
    kind: JobEventKind,
    scanned_seq: Option<u64>,
}

impl Jobs {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn job(&self, id: &JobId) -> Result<Job, RpcError> {
        self.lock()
            .jobs
            .get(id)
            .map(|s| s.job.clone())
            .ok_or_else(|| job_not_found(id))
    }

    /// Appends an event (applying it to the job) and sends it to the subscribers. `prepare`
    /// runs under the job lock and returns the event, or `None` to append nothing.
    async fn append(
        &self,
        host: &Host,
        job_id: &JobId,
        prepare: impl FnOnce(&mut State) -> Option<Change>,
    ) -> Result<Option<JobEvent>, RpcError> {
        let _log = self.log.lock().await;
        let (change, record, offset, scanned) = {
            let mut inner = self.lock();
            let state = inner.jobs.get_mut(job_id).ok_or_else(|| job_not_found(job_id))?;
            let Some(change) = prepare(state) else {
                return Ok(None);
            };
            if let Some(seq) = change.scanned_seq {
                state.scanned_seq = seq;
            }
            (
                change,
                serde_json::to_string(&state.job).map_err(internal)?,
                state.offset,
                state.scanned_seq,
            )
        };
        let at = UnixMillis::now();
        let id = job_id.to_string();
        let kind_json = serde_json::to_string(&change.kind).map_err(internal)?;
        let exit = matches!(change.kind, JobEventKind::Exit { .. });
        let rev = host
            .store
            .run(move |c| {
                let tx = c.transaction()?;
                let rev = meta_get(&tx, "jobs_rev")?
                    .and_then(|r| r.parse::<u64>().ok())
                    .unwrap_or(0)
                    + 1;
                meta_set(&tx, "jobs_rev", &rev.to_string())?;
                let rev_i = i64::try_from(rev).unwrap_or(i64::MAX);
                tx.execute(
                    "INSERT INTO job_events (rev, at, job_id, event) VALUES (?1, ?2, ?3, ?4)",
                    params![rev_i, at.0, id, kind_json],
                )?;
                tx.execute(
                    "UPDATE jobs SET record = ?2, scanned_seq = ?3, offset = ?4,
                     exit_rev = CASE WHEN ?5 THEN ?6 ELSE exit_rev END
                     WHERE id = ?1",
                    params![
                        id,
                        record,
                        i64::try_from(scanned).unwrap_or(i64::MAX),
                        i64::try_from(offset).unwrap_or(i64::MAX),
                        exit,
                        rev_i
                    ],
                )?;
                tx.commit()?;
                Ok(rev)
            })
            .await?;
        let event = JobEvent {
            rev: Rev(rev),
            at,
            job_id: *job_id,
            kind: change.kind,
        };
        self.broadcast(host, std::slice::from_ref(&event));
        Ok(Some(event))
    }

    fn broadcast(&self, host: &Host, events: &[JobEvent]) {
        let subscribers: Vec<(u64, SubscriptionId)> =
            self.lock().subscribers.iter().map(|(c, s)| (*c, *s)).collect();
        for (conn, subscription_id) in subscribers {
            match host.conns.get(conn) {
                Some(handle) => handle.notify::<notifications::JobsEvent>(JobsEventParams {
                    subscription_id,
                    events: events.to_vec(),
                }),
                None => {
                    self.lock().subscribers.remove(&conn);
                }
            }
        }
    }

    /// `jobs.subscribe` from connection `conn`.
    pub async fn subscribe(
        &self,
        host: &Host,
        conn: u64,
        params: JobsSubscribeParams,
    ) -> Result<JobsSubscribeResult, RpcError> {
        let _log = self.log.lock().await;
        let since = i64::try_from(params.since.map_or(0, |r| r.0)).unwrap_or(i64::MAX);
        let (rev, rows) = host
            .store
            .run(move |c| {
                let rev = meta_get(c, "jobs_rev")?
                    .and_then(|r| r.parse::<u64>().ok())
                    .unwrap_or(0);
                let mut stmt = c.prepare(
                    "SELECT rev, at, job_id, event FROM job_events WHERE rev > ?1 ORDER BY rev",
                )?;
                let rows = stmt
                    .query_map([since], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok((rev, rows))
            })
            .await?;
        let events = rows
            .into_iter()
            .filter_map(|(rev, at, job, event)| {
                Some(JobEvent {
                    rev: Rev(u64::try_from(rev).ok()?),
                    at: UnixMillis(at),
                    job_id: job.parse().ok()?,
                    kind: serde_json::from_str(&event).ok()?,
                })
            })
            .collect();
        let subscription_id = SubscriptionId::generate();
        self.lock().subscribers.insert(conn, subscription_id);
        Ok(JobsSubscribeResult {
            subscription_id,
            rev: Rev(rev),
            events,
        })
    }

    /// Ends connection `conn`'s subscription with this id; false if it has none.
    pub fn unsubscribe(&self, conn: u64, id: &SubscriptionId) -> bool {
        let mut inner = self.lock();
        if inner.subscribers.get(&conn) == Some(id) {
            inner.subscribers.remove(&conn);
            true
        } else {
            false
        }
    }

    /// `jobs.ack`: prunes the events of jobs that ended at or before `rev`.
    pub async fn ack(&self, host: &Host, params: JobsAckParams) -> Result<(), RpcError> {
        let rev = i64::try_from(params.rev.0).unwrap_or(i64::MAX);
        host.store
            .run(move |c| {
                let tx = c.transaction()?;
                let acked = meta_get(&tx, "jobs_acked")?
                    .and_then(|r| r.parse::<i64>().ok())
                    .unwrap_or(0)
                    .max(rev);
                meta_set(&tx, "jobs_acked", &acked.to_string())?;
                tx.execute(
                    "DELETE FROM job_events WHERE job_id IN
                     (SELECT id FROM jobs WHERE exit_rev IS NOT NULL AND exit_rev <= ?1)",
                    [acked],
                )?;
                tx.commit()
            })
            .await
    }

    /// Runs a `job_*` op of a tool call.
    pub async fn run_op(
        &self,
        host: &Arc<Host>,
        call_id: &CallId,
        workspace: &Arc<Workspace>,
        op: HostOp,
    ) -> Result<HostResult, RpcError> {
        match op {
            HostOp::JobRun {
                command,
                cwd,
                name,
                timeout_s,
                follow,
            } => {
                let job = self
                    .start(host, call_id, workspace, command, cwd, name, timeout_s, follow)
                    .await?;
                Ok(HostResult::JobRun { job: Box::new(job) })
            }
            HostOp::JobFollow {
                job_id,
                pattern,
                buffer_seconds,
                from,
            } => {
                self.owned(&job_id, workspace)?;
                let job = self
                    .follow(host, &job_id, pattern, buffer_seconds, from)
                    .await?;
                Ok(HostResult::JobFollow { job: Box::new(job) })
            }
            HostOp::JobUnfollow { job_id } => {
                self.owned(&job_id, workspace)?;
                self.append(host, &job_id, |state| {
                    state.job.follow.as_ref()?;
                    state.job.follow = None;
                    state.follow = None;
                    Some(Change {
                        kind: JobEventKind::Unfollow,
                        scanned_seq: None,
                    })
                })
                .await?;
                Ok(HostResult::JobUnfollow {
                    job: Box::new(self.job(&job_id)?),
                })
            }
            HostOp::JobKill { job_id, by } => {
                self.owned(&job_id, workspace)?;
                self.kill(host, &job_id, by).await?;
                Ok(HostResult::JobKill {
                    job: Box::new(self.job(&job_id)?),
                })
            }
            HostOp::JobStatus { job_id } => {
                let mut jobs: Vec<Job> = self
                    .lock()
                    .jobs
                    .values()
                    .filter(|s| s.job.workspace_id == Some(workspace.id))
                    .filter(|s| job_id.is_none_or(|id| s.job.id == id))
                    .map(|s| s.job.clone())
                    .collect();
                if let Some(id) = job_id
                    && jobs.is_empty()
                {
                    return Err(job_not_found(&id));
                }
                jobs.sort_by(|a, b| b.id.cmp(&a.id));
                Ok(HostResult::JobStatus { jobs })
            }
            _ => Err(invalid("not a job op")),
        }
    }

    fn owned(&self, id: &JobId, workspace: &Workspace) -> Result<(), RpcError> {
        match self.lock().jobs.get(id) {
            Some(s) if s.job.workspace_id == Some(workspace.id) => Ok(()),
            _ => Err(job_not_found(id)),
        }
    }

    #[allow(clippy::too_many_arguments, reason = "the fields of a job_run op")]
    async fn start(
        &self,
        host: &Arc<Host>,
        call_id: &CallId,
        workspace: &Arc<Workspace>,
        command: String,
        cwd: Option<RemotePath>,
        name: Option<String>,
        timeout_s: Option<u32>,
        follow: Option<JobFollowSpec>,
    ) -> Result<Job, RpcError> {
        if command.trim().is_empty() {
            return Err(invalid("the command is empty"));
        }
        let ctx = Context {
            root: workspace.root.to_string(),
            home: host.paths.dirs().home().to_string(),
        };
        let cwd = ctx.resolve_or_root(cwd.as_ref());
        if !Path::new(&cwd).starts_with(workspace.root.as_path()) {
            return Err(invalid("a job's cwd must be inside its workspace"));
        }
        let regex = follow
            .as_ref()
            .and_then(|f| f.pattern.as_deref())
            .map(compile)
            .transpose()?;
        let id = JobId::generate();
        let jobs_dir = workspace.root.join(".controlroom/jobs");
        let log_path = jobs_dir.join(&format!("{id}.log"));
        prepare_log_dir(&jobs_dir).map_err(|e| internal(format!("creating {jobs_dir}: {e}")))?;
        let name = name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| command.chars().take(40).collect());
        let started_at = UnixMillis::now();
        let job = Job {
            id,
            name: name.clone(),
            command: command.clone(),
            cwd: RemotePath::from(cwd.as_str()),
            machine_id: None,
            workspace_id: Some(workspace.id),
            status: JobStatus::Running,
            exit: None,
            signal: None,
            started_at,
            ended_at: None,
            timeout_s,
            log_path: RemotePath::from(log_path.as_str()),
            lines: 0,
            follow: follow.as_ref().map(|f| JobFollow {
                buffer_seconds: f.buffer_seconds,
                pattern: f.pattern.clone(),
                next_wake_at: None,
            }),
        };
        let record = serde_json::to_string(&job).map_err(internal)?;
        let (row_id, call, ws, session) = (
            id.to_string(),
            call_id.to_string(),
            workspace.id.to_string(),
            workspace.session_id.to_string(),
        );
        host.store
            .run(move |c| {
                c.execute(
                    "INSERT INTO jobs (id, call_id, workspace_id, session_id, record)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![row_id, call, ws, session, record],
                )
            })
            .await?;
        self.lock().jobs.insert(
            id,
            State {
                job: job.clone(),
                follow: regex,
                scanned_seq: 0,
                offset: 0,
                killed: false,
            },
        );
        let started = JobEventKind::Started {
            session_id: workspace.session_id,
            workspace_id: workspace.id,
            name,
            command: command.clone(),
            cwd: RemotePath::from(cwd.as_str()),
            log_path: RemotePath::from(log_path.as_str()),
            timeout_s,
            follow,
        };
        self.append(host, &id, |_| {
            Some(Change {
                kind: started,
                scanned_seq: None,
            })
        })
        .await?;
        match self
            .spawn(host, workspace, &id, &command, &cwd, timeout_s, &log_path)
            .await
        {
            Ok(socket) => {
                tokio::spawn(follow_job(host.clone(), id, socket, 1));
                self.job(&id)
            }
            Err(e) => {
                self.end(host, &id, None, Some(format!("it could not start: {}", e.message)))
                    .await?;
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments, reason = "everything a job supervisor needs")]
    async fn spawn(
        &self,
        host: &Arc<Host>,
        workspace: &Workspace,
        id: &JobId,
        command: &str,
        cwd: &str,
        timeout_s: Option<u32>,
        log_path: &controlroom_sandbox::AbsPath,
    ) -> Result<std::path::PathBuf, RpcError> {
        let argv = vec![
            login_shell().to_string(),
            "-lc".to_owned(),
            command.to_owned(),
        ];
        let wrapped = workspace.wrap(host, argv)?;
        let dir = host.paths.job_dir(&id.to_string());
        create_private_dir(&dir).map_err(internal)?;
        let env_file = dir.join("launch.env");
        let text: String = wrapped
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}\n"))
            .collect();
        write_private_file(&env_file, text.as_bytes()).map_err(internal)?;
        create_private_dir(&host.paths.dirs().run()).map_err(internal)?;
        let socket = host.paths.supervisor_socket(&id.to_string());
        let request = LaunchRequest {
            id: id.to_string(),
            paths: Paths {
                dir: dir.as_path().to_owned(),
                socket: socket.as_path().to_owned(),
            },
            spec: SpawnSpec {
                kind: ProcessKind::Job,
                exec: Exec::Argv(wrapped.argv),
                cwd: cwd.into(),
                env_file: Some(env_file.as_path().to_owned()),
                stdin: StdinMode::Null,
                timeout_ms: timeout_s.map(|s| u64::from(s.max(1)) * 1000),
                max_line_bytes: Some(MAX_LINE_BYTES),
                labels: BTreeMap::from([
                    ("job".to_owned(), id.to_string()),
                    ("workspace".to_owned(), workspace.id.to_string()),
                ]),
                plain_log: Some(log_path.as_path().to_owned()),
            },
        };
        launch(host.exe.as_path(), &request)
            .await
            .map_err(|e| internal(format!("starting the job: {e}")))?;
        Ok(socket.as_path().to_owned())
    }

    async fn follow(
        &self,
        host: &Arc<Host>,
        id: &JobId,
        pattern: Option<String>,
        buffer_seconds: Option<u32>,
        from: FollowFrom,
    ) -> Result<Job, RpcError> {
        let regex = pattern.as_deref().map(compile).transpose()?;
        let buffer_seconds = buffer_seconds.unwrap_or(DEFAULT_BUFFER_SECONDS).max(1);
        let transcript = host
            .paths
            .job_dir(&id.to_string())
            .join("transcript.jsonl");
        let lines = {
            let inner = self.lock();
            let state = inner.jobs.get(id).ok_or_else(|| job_not_found(id))?;
            if state.job.status != JobStatus::Running {
                return Err(invalid("the job has ended"));
            }
            state.job.lines
        };
        // Matches among the lines logged so far; later lines are matched by the scanner.
        let matches = if from == FollowFrom::Unseen {
            let regex = regex.clone();
            tokio::task::spawn_blocking(move || {
                rescan(transcript.as_path(), lines, regex.as_ref())
            })
            .await
            .map_err(internal)?
        } else {
            Vec::new()
        };
        self.append(host, id, |state| {
            state.follow = regex;
            state.job.follow = Some(JobFollow {
                buffer_seconds,
                pattern: pattern.clone(),
                next_wake_at: None,
            });
            Some(Change {
                kind: JobEventKind::Follow {
                    pattern,
                    buffer_seconds,
                    from,
                    matches,
                },
                scanned_seq: None,
            })
        })
        .await?;
        self.job(id)
    }

    async fn kill(&self, host: &Arc<Host>, id: &JobId, by: KilledBy) -> Result<(), RpcError> {
        let appended = self
            .append(host, id, |state| {
                if state.job.status != JobStatus::Running || state.killed {
                    return None;
                }
                state.killed = true;
                Some(Change {
                    kind: JobEventKind::Killed { by },
                    scanned_seq: None,
                })
            })
            .await?;
        if appended.is_some() {
            let socket = host.paths.supervisor_socket(&id.to_string());
            match Client::connect(socket.as_path(), true).await {
                Ok(client) => {
                    if let Err(e) = client.terminate(None).await {
                        tracing::warn!("killing job {id}: {e}");
                    }
                }
                Err(e) => tracing::warn!("killing job {id}: {e}"),
            }
        }
        Ok(())
    }

    /// Kills the running jobs of a closing workspace (as the user's kill).
    pub async fn kill_workspace(&self, host: &Arc<Host>, workspace: &WorkspaceId) {
        let ids: Vec<JobId> = self
            .lock()
            .jobs
            .values()
            .filter(|s| s.job.workspace_id == Some(*workspace) && s.job.status == JobStatus::Running)
            .map(|s| s.job.id)
            .collect();
        for id in ids {
            if let Err(e) = self.kill(host, &id, KilledBy::User).await {
                tracing::warn!("killing job {id}: {}", e.message);
            }
        }
    }

    /// Logs new lines: `raws` are the plain texts of output records up to `seq`.
    async fn progress(
        &self,
        host: &Host,
        id: &JobId,
        raws: &[String],
        seq: u64,
    ) -> Result<(), RpcError> {
        self.append(host, id, |state| {
            let first = state.job.lines + 1;
            let matches = match (&state.follow, &state.job.follow) {
                (_, None) => Vec::new(),
                (Some(regex), Some(_)) => (first..)
                    .zip(raws)
                    .filter(|(_, line)| regex.is_match(line))
                    .map(|(n, _)| n)
                    .collect(),
                (None, Some(_)) => (first..first + raws.len() as u64).collect(),
            };
            state.job.lines += raws.len() as u64;
            state.offset += raws.iter().map(|l| l.len() as u64 + 1).sum::<u64>();
            Some(Change {
                kind: JobEventKind::Progress {
                    lines: state.job.lines,
                    offset: state.offset,
                    matches,
                },
                scanned_seq: Some(seq),
            })
        })
        .await
        .map(drop)
    }

    /// Logs the job's end: its exit record, or why it is lost.
    async fn end(
        &self,
        host: &Host,
        id: &JobId,
        exit: Option<ExitInfo>,
        lost: Option<String>,
    ) -> Result<(), RpcError> {
        self.append(host, id, |state| {
            if state.job.status != JobStatus::Running {
                return None;
            }
            let now = UnixMillis::now();
            let (exit_code, signal) = exit.map_or((None, None), |e| (e.code, e.signal));
            let timed_out = exit.is_some_and(|e| e.cause == Some(StopCause::Timeout));
            state.job.status = match (exit_code, &lost) {
                (_, Some(_)) => JobStatus::Lost,
                _ if timed_out => JobStatus::TimedOut,
                _ if state.killed => JobStatus::Killed,
                (Some(0), None) => JobStatus::Succeeded,
                _ => JobStatus::Failed,
            };
            state.job.exit = exit_code;
            state.job.signal = signal.map(signal_name);
            state.job.ended_at = Some(now);
            state.job.follow = None;
            state.follow = None;
            Some(Change {
                kind: JobEventKind::Exit {
                    exit_code,
                    signal: signal.map(signal_name),
                    timed_out,
                    lost: lost.clone(),
                    lines: state.job.lines,
                    duration_ms: u64::try_from(now.0 - state.job.started_at.0).unwrap_or(0),
                },
                scanned_seq: None,
            })
        })
        .await
        .map(drop)
    }

    /// A `job_run` interrupted by a daemon restart: its job, if it was logged.
    pub async fn recover_run(&self, host: &Host, call_id: &CallId) -> Result<HostResult, RpcError> {
        let call = call_id.to_string();
        let job_id: Option<String> = host
            .store
            .run(move |c| {
                c.query_row("SELECT id FROM jobs WHERE call_id = ?1", [call], |r| {
                    r.get(0)
                })
                .optional()
            })
            .await?;
        match job_id.and_then(|id| id.parse().ok()).map(|id| self.job(&id)) {
            Some(Ok(job)) => Ok(HostResult::JobRun { job: Box::new(job) }),
            _ => Err(RpcError::new(
                ErrorData::Lost {
                    may_have_run: false,
                },
                "the host daemon restarted before this job was started; it is safe to retry",
            )),
        }
    }

    /// Loads the jobs and resumes following the running ones.
    pub async fn restore(&self, host: &Arc<Host>) -> Result<(), RpcError> {
        let rows: Vec<(String, i64, i64)> = host
            .store
            .run(|c| {
                let mut stmt = c.prepare("SELECT record, scanned_seq, offset FROM jobs")?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect()
            })
            .await?;
        let mut running = Vec::new();
        for (record, scanned, offset) in rows {
            let Ok(job) = serde_json::from_str::<Job>(&record) else {
                continue;
            };
            let follow = job
                .follow
                .as_ref()
                .and_then(|f| f.pattern.as_deref())
                .and_then(|p| compile(p).ok());
            if job.status == JobStatus::Running {
                running.push((job.id, u64::try_from(scanned).unwrap_or(0)));
            }
            self.lock().jobs.insert(
                job.id,
                State {
                    job,
                    follow,
                    scanned_seq: u64::try_from(scanned).unwrap_or(0),
                    offset: u64::try_from(offset).unwrap_or(0),
                    killed: false,
                },
            );
        }
        for (id, scanned) in running {
            let socket = host.paths.supervisor_socket(&id.to_string());
            tokio::spawn(follow_job(host.clone(), id, socket.as_path().to_owned(), scanned + 1));
        }
        Ok(())
    }
}

/// Creates the jobs dir and a `.controlroom/.gitignore` (`*`) if missing.
fn prepare_log_dir(jobs_dir: &controlroom_sandbox::AbsPath) -> std::io::Result<()> {
    std::fs::create_dir_all(jobs_dir.as_path())?;
    if let Some(parent) = jobs_dir.parent() {
        let ignore = parent.join(".gitignore");
        if !ignore.as_path().exists() {
            std::fs::write(ignore.as_path(), "*\n")?;
        }
    }
    Ok(())
}

/// Line numbers among the first `lines` output lines matching `regex` (all of them without).
fn rescan(transcript: &Path, lines: u64, regex: Option<&Regex>) -> Vec<u64> {
    let Ok(all) = controlroom_transcript::read_all(transcript, 1) else {
        return Vec::new();
    };
    output_lines(&all.records)
        .take(usize::try_from(lines).unwrap_or(usize::MAX))
        .enumerate()
        .filter(|(_, (_, line))| regex.is_none_or(|r| r.is_match(line)))
        .map(|(i, _)| i as u64 + 1)
        .collect()
}

/// The plain text of each output record, with its seq.
fn output_lines(records: &[Record]) -> impl Iterator<Item = (u64, String)> + '_ {
    records.iter().filter_map(|r| match &r.body {
        Body::Out(line) | Body::Stderr(line) => Some((r.seq, plain_line(&line.raw).into_owned())),
        _ => None,
    })
}

/// Follows a job's transcript from `from_seq` until its exit, logging progress and the exit.
async fn follow_job(host: Arc<Host>, id: JobId, socket: std::path::PathBuf, from_seq: u64) {
    let outcome = match Client::connect(&socket, false).await {
        Ok(client) => follow_live(&host, &id, &client, from_seq).await,
        Err(_) => Err(()),
    };
    if outcome.is_ok() {
        return;
    }
    // The supervisor is gone (or went away): finish from the transcript file.
    let transcript = host.paths.job_dir(&id.to_string()).join("transcript.jsonl");
    let from = host
        .jobs
        .lock()
        .jobs
        .get(&id)
        .map_or(from_seq, |s| s.scanned_seq + 1);
    let records = tokio::task::spawn_blocking(move || {
        controlroom_transcript::read_all(transcript.as_path(), from)
            .map(|a| a.records)
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    let mut batch = Vec::new();
    let mut seq = from.saturating_sub(1);
    let mut exit = None;
    for record in &records {
        match &record.body {
            Body::Out(line) | Body::Stderr(line) => batch.push(plain_line(&line.raw).into_owned()),
            Body::Exit(info) => exit = Some(*info),
            _ => {}
        }
        seq = record.seq;
    }
    let jobs = &host.jobs;
    if !batch.is_empty()
        && let Err(e) = jobs.progress(&host, &id, &batch, seq).await
    {
        tracing::warn!("job {id}: {}", e.message);
    }
    let lost = exit
        .is_none()
        .then(|| "its supervisor is gone (the host rebooted?)".to_owned());
    if let Err(e) = jobs.end(&host, &id, exit, lost).await {
        tracing::warn!("job {id}: {}", e.message);
    }
}

/// Follows a live supervisor; `Err` if the stream ended without an exit record.
async fn follow_live(host: &Arc<Host>, id: &JobId, client: &Client, from_seq: u64) -> Result<(), ()> {
    let mut stream = client.read(from_seq).await.map_err(drop)?;
    let jobs = &host.jobs;
    let mut batch = Vec::new();
    let mut seq = from_seq.saturating_sub(1);
    let mut tick = tokio::time::interval(PROGRESS_EVERY);
    loop {
        tokio::select! {
            record = stream.next() => {
                let Some(record) = record else {
                    flush(host, id, &mut batch, seq).await;
                    return Err(());
                };
                match record.body {
                    Body::Out(line) | Body::Stderr(line) => {
                        batch.push(plain_line(&line.raw).into_owned());
                    }
                    Body::Exit(exit) => {
                        seq = record.seq;
                        flush(host, id, &mut batch, seq).await;
                        if let Err(e) = jobs.end(host, id, Some(exit), None).await {
                            tracing::warn!("job {id}: {}", e.message);
                        }
                        return Ok(());
                    }
                    _ => {}
                }
                seq = record.seq;
            }
            _ = tick.tick() => flush(host, id, &mut batch, seq).await,
        }
    }
}

async fn flush(host: &Host, id: &JobId, batch: &mut Vec<String>, seq: u64) {
    if batch.is_empty() {
        return;
    }
    if let Err(e) = host.jobs.progress(host, id, batch, seq).await {
        tracing::warn!("job {id}: {}", e.message);
    }
    batch.clear();
}

#[cfg(test)]
mod tests {
    use controlroom_transcript::Line;

    use super::*;

    #[test]
    fn rescans_output_lines() {
        let records: Vec<Record> = ["ok", "\u{1b}[31mERROR\u{1b}[0m one", "fine", "ERROR two"]
            .iter()
            .enumerate()
            .map(|(i, raw)| Record {
                seq: i as u64 + 2,
                ts: 0,
                body: Body::Out(Line::new(*raw)),
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let text: String = records
            .iter()
            .map(|r| serde_json::to_string(r).unwrap() + "\n")
            .collect();
        std::fs::write(&path, text).unwrap();
        let regex = compile("^ERROR").unwrap();
        assert_eq!(rescan(&path, 4, Some(&regex)), [2, 4]);
        assert_eq!(rescan(&path, 3, Some(&regex)), [2]);
        assert_eq!(rescan(&path, 2, None), [1, 2]);
        assert!(compile(&"a".repeat(2000)).is_err());
    }
}
