//! Tool calls (`tool.run`, `tool.attach`, `tool.cancel`, `journal.ack`) and their journal in
//! `host.sqlite`, keyed by call id (design §3.4, `docs/protocol.md` §6 "Exactly once").
//!
//! - A repeated `tool.run` (or `tool.attach`) of a running call **attaches** to it; of a
//!   finished one it returns the journaled outcome. Nothing ever runs twice.
//! - Reads (`read`, `read_bytes`, `stat`, `ls`, `find`, `grep`) are journaled when they end;
//!   after a daemon restart a resend runs them again (harmless).
//! - `write` journals its intent (the resolved path and the new content's hash, fsynced)
//!   before the fs worker writes. After a restart mid-write the file's hash decides: the new
//!   content → the result; otherwise `lost {may_have_run: false}` (safe to retry).
//! - `bash` journals its shell and the transcript position where its output starts before the
//!   command reaches the shell. After a restart, a command whose shell survived is followed to
//!   its end from the shell's transcript; otherwise it is `lost {may_have_run: true}`.
//! - Job ops are journaled before they act; a `job_run` whose job exists is recovered, other
//!   interrupted job ops are `lost`.
//! - Rows go when the agent daemon acks them (`journal.ack`) or 24 h after they ended.
//!
//! `tool.output` and `tool.waiting_input` go to every connection waiting on the call.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use controlroom_protocol::host::{
    HostOp, HostResult, JournalAckParams, ToolCallRef, ToolCancelResult, ToolOutputParams,
    ToolRunParams, ToolWaitingInputParams, notifications,
};
use controlroom_protocol::ids::{CallId, UnixMillis, WorkspaceId};
use controlroom_protocol::{ErrorData, RemotePath, Resource, RpcError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, watch};

use super::Host;
use super::conns::Conns;
use super::fsops::{Context, sha256_hex};
use super::worker::WorkerClient;
use super::shells::{self, Resume};
use super::store::internal;

/// Default `bash` timeout.
const DEFAULT_TIMEOUT_S: u32 = 600;
/// How long finished rows (and command logs) are kept without an ack.
const RETENTION: Duration = Duration::from_secs(24 * 3600);

/// A finished call's outcome, as journaled.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Ok(HostResult),
    Err(RpcError),
}

impl Outcome {
    fn from(result: Result<HostResult, RpcError>) -> Self {
        match result {
            Ok(result) => Outcome::Ok(result),
            Err(error) => Outcome::Err(error),
        }
    }

    fn into_result(self) -> Result<HostResult, RpcError> {
        match self {
            Outcome::Ok(result) => Ok(result),
            Outcome::Err(error) => Err(error),
        }
    }
}

/// What a running call's journal row remembers for recovery (`intent`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Intent {
    Write {
        path: String,
        sha256: String,
        bytes: u64,
    },
    Bash {
        timeout_s: u32,
        shell_reset: Option<String>,
    },
    Job,
}

/// A call in progress.
struct Running {
    result: watch::Sender<Option<Result<HostResult, RpcError>>>,
    listeners: Mutex<HashSet<u64>>,
    cancel: Arc<Notify>,
    bash: bool,
}

/// The calls in progress.
pub struct Tools {
    running: Mutex<HashMap<CallId, Arc<Running>>>,
    conns: Arc<Conns>,
}

fn lost(may_have_run: bool, message: &str) -> RpcError {
    RpcError::new(ErrorData::Lost { may_have_run }, message)
}

fn not_found(call_id: &CallId) -> RpcError {
    RpcError::new(
        ErrorData::NotFound {
            resource: Resource::Call,
            id: call_id.to_string(),
        },
        "this host never started that call",
    )
}

/// The op's kind (`"read"`, `"bash"`, ...).
fn op_name(op: &HostOp) -> String {
    serde_json::to_value(op)
        .ok()
        .and_then(|v| v["op"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn args_hash(workspace: &WorkspaceId, op: &HostOp) -> String {
    let text = serde_json::to_string(&(workspace, op)).unwrap_or_default();
    sha256_hex(text.as_bytes())
}

/// A journal row.
struct Row {
    args_hash: String,
    outcome: Option<String>,
}

impl Tools {
    /// No calls yet; notifications go through `conns`.
    pub fn new(conns: Arc<Conns>) -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
            conns,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<CallId, Arc<Running>>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The running call with this id.
    fn running(&self, call_id: &CallId) -> Option<Arc<Running>> {
        self.lock().get(call_id).cloned()
    }

    /// Streams new output of a running call to the connections waiting on it.
    pub fn output(&self, call_id: &CallId, text: String) {
        self.notify(call_id, |conn| {
            conn.notify::<notifications::ToolOutput>(ToolOutputParams {
                call_id: call_id.clone(),
                text: text.clone(),
            });
        });
    }

    /// Reports that a running call started or stopped waiting for terminal input.
    pub fn waiting(&self, call_id: &CallId, waiting: bool) {
        self.notify(call_id, |conn| {
            conn.notify::<notifications::ToolWaitingInput>(ToolWaitingInputParams {
                call_id: call_id.clone(),
                waiting,
            });
        });
    }

    fn notify(&self, call_id: &CallId, send: impl Fn(&super::conns::ConnHandle)) {
        let Some(running) = self.lock().get(call_id).cloned() else {
            return;
        };
        let listeners: Vec<u64> = running
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .copied()
            .collect();
        for conn in listeners {
            if let Some(handle) = self.conns.get(conn) {
                send(&handle);
            }
        }
    }

    /// Waits for a running call's outcome, with `conn` getting its notifications meanwhile.
    async fn wait(running: Arc<Running>, conn: u64) -> Result<HostResult, RpcError> {
        running
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(conn);
        let mut rx = running.result.subscribe();
        let outcome = rx
            .wait_for(Option::is_some)
            .await
            .map_err(|_| internal("the call's runner vanished"))?
            .clone()
            .expect("waited for a value");
        running
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&conn);
        outcome
    }

    /// Registers a new call as running; `None` if one with this id already runs.
    fn register(&self, call_id: &CallId, bash: bool) -> Option<Arc<Running>> {
        let mut running = self.lock();
        if running.contains_key(call_id) {
            return None;
        }
        let entry = Arc::new(Running {
            result: watch::Sender::new(None),
            listeners: Mutex::new(HashSet::new()),
            cancel: Arc::new(Notify::new()),
            bash,
        });
        running.insert(call_id.clone(), entry.clone());
        Some(entry)
    }

    /// Journals a call's outcome, then releases its waiters.
    async fn finish(&self, host: &Host, call_id: &CallId, result: Result<HostResult, RpcError>) {
        let state = match &result {
            Err(RpcError {
                data: Some(ErrorData::Lost { .. }),
                ..
            }) => "lost",
            _ => "done",
        };
        let outcome = serde_json::to_string(&Outcome::from(result.clone())).unwrap_or_default();
        let id = call_id.to_string();
        let saved = host
            .store
            .run(move |c| {
                c.execute(
                    "UPDATE journal SET state = ?2, outcome = ?3, ended_at = ?4
                     WHERE call_id = ?1",
                    params![id, state, outcome, UnixMillis::now().0],
                )
            })
            .await;
        if let Err(e) = saved {
            tracing::error!("journaling call {call_id}: {}", e.message);
        }
        if let Some(running) = self.lock().remove(call_id) {
            running.result.send_replace(Some(result));
        }
    }
}

async fn row(host: &Host, call_id: &CallId) -> Result<Option<Row>, RpcError> {
    let id = call_id.to_string();
    host.store
        .run(move |c| {
            c.query_row(
                "SELECT args_hash, outcome FROM journal WHERE call_id = ?1",
                [id],
                |r| {
                    Ok(Row {
                        args_hash: r.get(0)?,
                        outcome: r.get(1)?,
                    })
                },
            )
            .optional()
        })
        .await
}

fn journaled(row: Row) -> Result<HostResult, RpcError> {
    match row.outcome.and_then(|o| serde_json::from_str::<Outcome>(&o).ok()) {
        Some(outcome) => outcome.into_result(),
        None => Err(lost(
            true,
            "the host daemon restarted while this call ran and its result is gone",
        )),
    }
}

/// Inserts a running call's journal row (fsynced before the call acts).
async fn insert(
    host: &Host,
    call_id: &CallId,
    workspace: WorkspaceId,
    op: &HostOp,
    shell: Option<(String, u64)>,
    intent: Option<Intent>,
) -> Result<(), RpcError> {
    let (id, ws, name, hash) = (
        call_id.to_string(),
        workspace.to_string(),
        op_name(op),
        args_hash(&workspace, op),
    );
    let intent = intent.map(|i| serde_json::to_string(&i).unwrap_or_default());
    let (shell_id, start_seq) = shell.unzip();
    host.store
        .run(move |c| {
            c.execute(
                "INSERT INTO journal
                 (call_id, workspace_id, op, args_hash, state, shell_id, start_seq, intent,
                  started_at)
                 VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7, ?8)",
                params![
                    id,
                    ws,
                    name,
                    hash,
                    shell_id,
                    start_seq.map(|s| i64::try_from(s).unwrap_or(i64::MAX)),
                    intent,
                    UnixMillis::now().0
                ],
            )
        })
        .await?;
    Ok(())
}

/// `tool.run`.
pub async fn run(host: &Arc<Host>, conn: u64, params: ToolRunParams) -> Result<HostResult, RpcError> {
    let ToolRunParams {
        call_id,
        workspace_id,
        op,
    } = params;
    if let Some(running) = host.tools.running(&call_id) {
        return Tools::wait(running, conn).await;
    }
    if let Some(row) = row(host, &call_id).await? {
        if row.args_hash != args_hash(&workspace_id, &op) {
            return Err(RpcError::new(
                ErrorData::InvalidParams,
                format!("call id {call_id} was already used for a different call"),
            ));
        }
        if let Some(running) = host.tools.running(&call_id) {
            return Tools::wait(running, conn).await;
        }
        return journaled(row);
    }
    if op == HostOp::Unknown {
        return Err(RpcError::new(
            ErrorData::Unsupported,
            "this host doesn't know that op; upgrade it",
        ));
    }
    let workspace = host.workspaces.require(&workspace_id)?;
    let Some(running) = host
        .tools
        .register(&call_id, matches!(op, HostOp::Bash { .. }))
    else {
        // A concurrent duplicate won the race.
        let running = host.tools.running(&call_id);
        return match running {
            Some(running) => Tools::wait(running, conn).await,
            None => run_again(host, conn, &call_id).await,
        };
    };
    let task_host = host.clone();
    let task_call = call_id.clone();
    let cancel = running.cancel.clone();
    tokio::spawn(async move {
        let result = execute(&task_host, &task_call, workspace, op, cancel).await;
        task_host.tools.finish(&task_host, &task_call, result).await;
    });
    Tools::wait(running, conn).await
}

/// The outcome of a call that ended between two checks.
async fn run_again(host: &Arc<Host>, conn: u64, call_id: &CallId) -> Result<HostResult, RpcError> {
    attach(host, conn, ToolCallRef {
        call_id: call_id.clone(),
    })
    .await
}

/// Runs a new call (its runner task, detached from the requesting connection).
async fn execute(
    host: &Arc<Host>,
    call_id: &CallId,
    workspace: Arc<super::workspaces::Workspace>,
    op: HostOp,
    cancel: Arc<Notify>,
) -> Result<HostResult, RpcError> {
    let ws = workspace.id;
    match op {
        HostOp::Read { .. }
        | HostOp::ReadBytes { .. }
        | HostOp::Stat { .. }
        | HostOp::Ls { .. }
        | HostOp::Find { .. }
        | HostOp::Grep { .. } => {
            insert(host, call_id, ws, &op, None, None).await?;
            workspace.worker(host)?.call(op).await
        }
        HostOp::Write {
            ref path,
            ref content,
            encoding,
            ..
        } => {
            let ctx = Context {
                root: workspace.root.to_string(),
                home: host.paths.dirs().home().to_string(),
            };
            let data = super::fsops::content_bytes(content, encoding)?;
            let intent = Intent::Write {
                path: ctx.resolve(path.as_str()),
                sha256: sha256_hex(&data),
                bytes: data.len() as u64,
            };
            insert(host, call_id, ws, &op, None, Some(intent)).await?;
            workspace.worker(host)?.call(op).await
        }
        HostOp::Bash {
            ref shell,
            ref command,
            timeout_s,
        } => {
            let shell = host.shells.require(workspace.session_id, shell)?;
            let timeout_s = timeout_s.unwrap_or(DEFAULT_TIMEOUT_S);
            let started = shells::run::start(
                host,
                &shell,
                call_id,
                command,
                timeout_s,
                |start_seq, shell_reset| {
                    let intent = Intent::Bash {
                        timeout_s,
                        shell_reset,
                    };
                    let place = Some((shell.id.to_string(), start_seq));
                    insert(host, call_id, ws, &op, place, Some(intent))
                },
            )
            .await;
            match started {
                Ok(run) => Ok(shells::run::drive(host, run, cancel).await),
                Err(e) => {
                    // Without a journal row the error is journaled by `finish` only if the
                    // row exists; make sure it does, so a resend sees the same error.
                    let _ = insert(host, call_id, ws, &op, None, None).await;
                    Err(e)
                }
            }
        }
        HostOp::JobRun { .. }
        | HostOp::JobFollow { .. }
        | HostOp::JobUnfollow { .. }
        | HostOp::JobKill { .. }
        | HostOp::JobStatus { .. } => {
            insert(host, call_id, ws, &op, None, Some(Intent::Job)).await?;
            host.jobs.run_op(host, call_id, &workspace, op).await
        }
        HostOp::Unknown => Err(RpcError::new(ErrorData::Unsupported, "unknown op")),
    }
}

/// `tool.attach`.
pub async fn attach(
    host: &Arc<Host>,
    conn: u64,
    params: ToolCallRef,
) -> Result<HostResult, RpcError> {
    if let Some(running) = host.tools.running(&params.call_id) {
        return Tools::wait(running, conn).await;
    }
    match row(host, &params.call_id).await? {
        Some(row) => match host.tools.running(&params.call_id) {
            Some(running) => Tools::wait(running, conn).await,
            None => journaled(row),
        },
        None => Err(not_found(&params.call_id)),
    }
}

/// `tool.cancel`.
pub fn cancel(host: &Host, params: &ToolCallRef) -> ToolCancelResult {
    let running = host.tools.running(&params.call_id);
    match running {
        Some(running) if running.bash => {
            running.cancel.notify_one();
            ToolCancelResult { cancelled: true }
        }
        _ => ToolCancelResult { cancelled: false },
    }
}

/// `journal.ack`: forgets finished calls the agent daemon has logged.
pub async fn ack(host: &Host, params: JournalAckParams) -> Result<(), RpcError> {
    let ids: Vec<String> = params.call_ids.iter().map(ToString::to_string).collect();
    host.store
        .run(move |c| {
            let tx = c.transaction()?;
            for id in ids {
                tx.execute(
                    "DELETE FROM journal WHERE call_id = ?1 AND state != 'running'",
                    [id],
                )?;
            }
            tx.commit()
        })
        .await
}

/// Drops finished rows and command files older than the retention.
pub async fn prune(host: &Host) {
    let cutoff = UnixMillis::now().0 - i64::try_from(RETENTION.as_millis()).unwrap_or(i64::MAX);
    let pruned = host
        .store
        .run(move |c| {
            c.execute(
                "DELETE FROM journal WHERE state != 'running' AND ended_at < ?1",
                [cutoff],
            )
        })
        .await;
    if let Err(e) = pruned {
        tracing::warn!("pruning the journal: {}", e.message);
    }
    let views: Vec<_> = host
        .workspaces
        .all()
        .iter()
        .map(|w| w.files.view())
        .collect();
    let _ = tokio::task::spawn_blocking(move || {
        for view in views {
            for sub in ["cmds", "calls"] {
                remove_older_than(&view.as_path().join(sub), RETENTION);
            }
        }
    })
    .await;
}

fn remove_older_than(dir: &std::path::Path, age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|e| e > age);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Resolves the calls a previous daemon left running (see the module docs). Runs before the
/// daemon serves.
pub async fn recover(host: &Arc<Host>) -> Result<(), RpcError> {
    type Pending = (String, String, String, Option<String>, Option<i64>, Option<String>, i64);
    let rows: Vec<Pending> = host
        .store
        .run(|c| {
            let mut stmt = c.prepare(
                "SELECT call_id, workspace_id, op, shell_id, start_seq, intent, started_at
                 FROM journal WHERE state = 'running'",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?;
            rows.collect()
        })
        .await?;
    let mut resumes = HashMap::new();
    let mut outcomes: Vec<(CallId, Result<HostResult, RpcError>)> = Vec::new();
    for (call_id, workspace, op, shell_id, start_seq, intent, started_at) in rows {
        let call_id = CallId::new(call_id);
        let intent = intent.and_then(|i| serde_json::from_str::<Intent>(&i).ok());
        match (op.as_str(), intent) {
            ("bash", Some(Intent::Bash {
                timeout_s,
                shell_reset,
            })) if shell_id.is_some() && start_seq.is_some() => {
                let shell_id = controlroom_protocol::ids::ShellId::new(shell_id.unwrap_or_default());
                resumes.insert(
                    shell_id,
                    Resume {
                        call_id,
                        start_seq: u64::try_from(start_seq.unwrap_or(1)).unwrap_or(1),
                        started_at: UnixMillis(started_at),
                        timeout_s,
                        shell_reset,
                    },
                );
            }
            ("write", Some(Intent::Write {
                path,
                sha256,
                bytes,
            })) => {
                let outcome = recover_write(host, &workspace, &path, &sha256, bytes).await;
                outcomes.push((call_id, outcome));
            }
            ("job_run", _) => {
                let outcome = host.jobs.recover_run(host, &call_id).await;
                outcomes.push((call_id, outcome));
            }
            _ => outcomes.push((
                call_id,
                Err(lost(
                    true,
                    "the host daemon restarted while this call ran; it may have (partly) run",
                )),
            )),
        }
    }
    let resumed_calls: HashSet<CallId> = resumes.values().map(|r| r.call_id.clone()).collect();
    let adopted = host.shells.restore(host, resumes).await?;
    for call_id in resumed_calls {
        if !adopted.contains_key(&call_id) {
            outcomes.push((
                call_id,
                Err(lost(
                    true,
                    "the shell died with the host daemon while this command ran; it may have \
                     (partly) run",
                )),
            ));
        }
    }
    for (call_id, outcome) in outcomes {
        host.tools.finish(host, &call_id, outcome).await;
    }
    for (call_id, run) in adopted {
        let Some(running) = host.tools.register(&call_id, true) else {
            continue;
        };
        let host = host.clone();
        tokio::spawn(async move {
            let result = shells::run::drive(&host, run, running.cancel.clone()).await;
            host.tools.finish(&host, &call_id, Ok(result)).await;
        });
    }
    Ok(())
}

/// The SHA-256 of a file as the fs worker reads it (`None`: missing, unreadable, or too large
/// to read whole).
async fn file_digest(worker: &WorkerClient, path: &str) -> Option<String> {
    match worker
        .call(HostOp::ReadBytes {
            path: RemotePath::from(path),
            offset: None,
            length: None,
        })
        .await
    {
        Ok(HostResult::ReadBytes { sha256, .. }) => sha256,
        _ => None,
    }
}

/// A write interrupted by a restart: applied iff the file holds the new content.
async fn recover_write(
    host: &Arc<Host>,
    workspace: &str,
    path: &str,
    sha256: &str,
    bytes: u64,
) -> Result<HostResult, RpcError> {
    let not_applied = || {
        lost(
            false,
            "the host daemon restarted during this write and the file doesn't hold the new \
             content; it is safe to retry",
        )
    };
    let Some(workspace) = workspace
        .parse()
        .ok()
        .and_then(|id| host.workspaces.get(&id))
    else {
        return Err(not_applied());
    };
    let digest = match workspace.worker(host) {
        Ok(worker) => file_digest(&worker, path).await,
        Err(_) => None,
    };
    if digest.as_deref() == Some(sha256) {
        Ok(HostResult::Write {
            path: RemotePath::from(path),
            bytes,
            sha256: sha256.to_owned(),
        })
    } else {
        Err(not_applied())
    }
}
