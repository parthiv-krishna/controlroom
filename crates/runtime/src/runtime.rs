//! [`Runtime`]: the public handle the daemon serves the daemon API with.

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use controlroom_protocol::RpcError;
use controlroom_protocol::daemon::{
    DaemonSettings, MachineChange, MachineSnapshot, PromptResult, QueueEditParams,
    QueueRemoveParams, QueueReorderParams, QueueSendNowParams, ReadCursor, RequestRespondParams,
    RequestRespondResult, RetentionPolicy, SendNowResult, SessionCreateParams, SessionCreateResult,
    SessionMarkReadParams, SessionOpParams, SessionPromptParams, SessionResumeParams,
    SessionSetOptionParams, SessionStartFreshParams, SessionUpdateParams, SettingsSetParams,
    StartFreshResult,
};
use controlroom_protocol::ids::{DaemonId, Rev, Seq, SessionId};
use controlroom_protocol::session::{RequestState, SessionDetail, SessionSummary};
use controlroom_protocol::timeline::{RawFrame, TimelineItem};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{broadcast, watch};
use tokio::task::{AbortHandle, JoinSet};

use crate::commands::params_hash;
use crate::core::Core;
use crate::errors::{busy, internal};
use crate::io;
use crate::ports::{Agents, Clock, Host};
use crate::raised::{RaiseRequest, RaisedRequest};
use crate::session::{Session, SessionUpdate, raw_frame};
use crate::store::{MigrateError, Store};

/// What the runtime needs to open.
pub struct RuntimeConfig {
    /// `daemon.sqlite`.
    pub db_path: PathBuf,
    /// Supervisors and session files.
    pub host: Arc<dyn Host>,
    /// The configured agents.
    pub agents: Arc<dyn Agents>,
    /// The time.
    pub clock: Arc<dyn Clock>,
}

/// Why the runtime didn't open.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The database couldn't be opened or migrated.
    #[error("opening the daemon database: {0}")]
    Database(String),
    /// Reading it failed.
    #[error("reading the daemon database: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// `machine.subscribe`: either a snapshot or the changes the client missed, then live changes.
pub struct MachineSubscription {
    /// The rev the snapshot or backlog reaches.
    pub rev: Rev,
    /// The full state, when the client can't catch up incrementally.
    pub snapshot: Option<MachineSnapshot>,
    /// The changes after the client's `since_rev` (without a snapshot).
    pub backlog: Vec<MachineChange>,
    /// Live changes after `rev`. A lagging receiver must resubscribe from its last rev.
    pub changes: broadcast::Receiver<MachineChange>,
}

/// `session.subscribe`: the items changed after `since_seq`, then live updates.
pub struct SessionSubscription {
    /// The transcript head the backlog reaches.
    pub head_seq: Seq,
    /// The session detail at `head_seq`.
    pub detail: SessionDetail,
    /// Every item changed after `since_seq`, ordered by `seq_from` (send as upserts).
    pub backlog: Vec<TimelineItem>,
    /// Raw frames after `raw_since` up to `head_seq`, when asked for.
    pub raw: Vec<RawFrame>,
    /// Live updates after `head_seq`. A lagging receiver must resubscribe from its last
    /// `head_seq`.
    pub updates: broadcast::Receiver<SessionUpdate>,
}

/// Shared state of a runtime and its tasks.
pub(crate) struct Inner {
    core: Mutex<Core>,
    pub host: Arc<dyn Host>,
    pub agents: Arc<dyn Agents>,
    tasks: Mutex<JoinSet<()>>,
    recovered: watch::Sender<bool>,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `f` under the lock, outside any transaction.
    pub(crate) fn with_core<R>(&self, f: impl FnOnce(&mut Core) -> R) -> R {
        f(&mut self.lock())
    }

    /// Runs `f` as one transaction and starts the tasks it asked for.
    pub(crate) fn transact<R>(
        self: &Arc<Self>,
        f: impl FnOnce(&mut Core) -> Result<R, RpcError>,
    ) -> Result<R, RpcError> {
        let (result, tasks) = self.lock().transact(f);
        io::spawn_tasks(self, tasks);
        result
    }

    pub(crate) fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) -> AbortHandle {
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        while tasks.try_join_next().is_some() {}
        tasks.spawn(task)
    }
}

/// The session runtime: registry, queue, requests, reconcile, recovery and feeds (spec §6, §8).
/// Cheap to clone; every clone is the same runtime.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
}

impl Runtime {
    /// Opens the store and loads every session. Call [`Runtime::recover`] next (after binding
    /// the socket): commands wait until it has finished.
    pub fn open(config: RuntimeConfig) -> Result<Runtime, OpenError> {
        let store = Store::open(&config.db_path)?;
        let daemon_id = match store.kv_get("daemon_id")? {
            Some(id) => id,
            None => {
                let id = DaemonId::generate();
                store.kv_set("daemon_id", &id)?;
                id
            }
        };
        let settings = store.kv_get("settings")?.unwrap_or(DaemonSettings {
            idle_timeout_h: 48,
            retention: RetentionPolicy {
                delete_closed_after_days: None,
            },
        });
        let rev = store.last_rev()?;
        let sessions = store.load_sessions()?;
        let mut core = Core::new(
            store,
            settings,
            daemon_id,
            rev,
            config.clock,
            config.agents.clone(),
        );
        for data in sessions {
            let driver = config.agents.driver(data.driver);
            let mut session = Session::new(data, driver);
            session.live.loading = true;
            core.sessions.insert(session.data.id, session);
        }
        Ok(Runtime {
            inner: Arc::new(Inner {
                core: Mutex::new(core),
                host: config.host,
                agents: config.agents,
                tasks: Mutex::new(JoinSet::new()),
                recovered: watch::channel(false).0,
            }),
        })
    }

    /// Startup recovery (spec §6.6): re-folds every transcript and reattaches live
    /// supervisors, concurrently. Sessions whose supervisor is gone become `interrupted` or
    /// `dormant`; a start that never finished is retried. Requests the previous daemon raised
    /// and nobody answered expire: whoever waited for them is gone.
    pub async fn recover(&self) {
        let ids: Vec<SessionId> = self
            .inner
            .with_core(|core| core.sessions.keys().copied().collect());
        let recoveries = ids
            .into_iter()
            .map(|id| io::recover_session(self.inner.clone(), id));
        futures::future::join_all(recoveries).await;
        let finished = self.inner.transact(|core| {
            let ids: Vec<SessionId> = core.sessions.keys().copied().collect();
            for id in ids {
                core.end_pending_raised(id, RequestState::Expired)?;
                core.touch(id);
            }
            Ok(())
        });
        if let Err(e) = finished {
            tracing::warn!("finishing recovery: {e}");
        }
        self.inner.recovered.send_replace(true);
    }

    /// Stops every task and drops every supervisor connection (agents keep running).
    pub async fn shutdown(&self) {
        let mut tasks = {
            let mut tasks = self
                .inner
                .tasks
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            std::mem::take(&mut *tasks)
        };
        self.inner.with_core(|core| {
            for session in core.sessions.values_mut() {
                session.abort_tasks();
                session.live.attachment = None;
            }
        });
        tasks.shutdown().await;
    }

    /// This daemon's id (generated on first open).
    pub fn daemon_id(&self) -> DaemonId {
        self.inner.with_core(|core| core.daemon_id)
    }

    /// Periodic work; the daemon calls it about once a second.
    pub fn tick(&self) {
        if let Err(e) = self.inner.transact(Core::tick) {
            tracing::warn!("tick: {e}");
        }
    }

    async fn recovered(&self) {
        let mut recovered = self.inner.recovered.subscribe();
        let _ = recovered.wait_for(|done| *done).await;
    }

    /// Runs a mutating command once per op id.
    async fn command<P: Serialize, R: Serialize + DeserializeOwned>(
        &self,
        method: &str,
        op_id: &controlroom_protocol::ids::OpId,
        params: &P,
        f: impl FnOnce(&mut Core) -> Result<R, RpcError>,
    ) -> Result<R, RpcError> {
        self.recovered().await;
        self.inner
            .transact(|core| core.with_op(method, op_id, params, f))
    }

    // -----------------------------------------------------------------------------------------
    // Feeds
    // -----------------------------------------------------------------------------------------

    /// One session's summary.
    pub fn summary(&self, id: SessionId) -> Result<SessionSummary, RpcError> {
        self.inner.with_core(|core| {
            core.session(id)?;
            Ok(core.summary(id))
        })
    }

    /// `machine.subscribe`.
    pub fn subscribe_machine(&self, since: Option<Rev>) -> Result<MachineSubscription, RpcError> {
        self.inner.with_core(|core| {
            let changes = core.machine_tx.subscribe();
            let rev = core.rev;
            let first = core.store.first_rev().map_err(internal)?;
            let reachable = since.is_some_and(|since| {
                since <= rev && (since == rev || first.is_some_and(|f| f.0 <= since.0 + 1))
            });
            if let (true, Some(since)) = (reachable, since) {
                let backlog = core.store.revs_after(since).map_err(internal)?;
                return Ok(MachineSubscription {
                    rev,
                    snapshot: None,
                    backlog,
                    changes,
                });
            }
            let sessions = core.sessions.keys().map(|id| core.summary(*id)).collect();
            Ok(MachineSubscription {
                rev,
                snapshot: Some(MachineSnapshot {
                    sessions,
                    settings: core.settings.clone(),
                }),
                backlog: Vec::new(),
                changes,
            })
        })
    }

    /// `session.subscribe`, served from the materialized timeline. `since: None` asks for the
    /// detail only (no timeline backlog).
    pub async fn subscribe_session(
        &self,
        id: SessionId,
        since: Option<Seq>,
        raw_since: Option<Seq>,
    ) -> Result<SessionSubscription, RpcError> {
        self.recovered().await;
        let (head_seq, detail, backlog, updates) = self.inner.transact(|core| {
            let session = core.session(id)?;
            if session.live.loading {
                return Err(busy("the session is loading; try again"));
            }
            let head = core.head(id);
            let backlog = match since {
                Some(since) => {
                    core.flush_timeline(id)?;
                    let since = if since > head { Seq(0) } else { since };
                    core.store.timeline_since(id, since).map_err(internal)?
                }
                None => Vec::new(),
            };
            let session = core.session(id)?;
            Ok((head, session.detail(), backlog, session.updates.subscribe()))
        })?;
        let mut raw = Vec::new();
        if let Some(raw_since) = raw_since.filter(|r| *r < head_seq) {
            let mut stream = self
                .inner
                .host
                .read_transcript(id, Seq(raw_since.0 + 1))
                .await
                .map_err(internal)?;
            while let Some(record) = stream.next().await {
                if record.seq > head_seq.0 {
                    break;
                }
                raw.push(raw_frame(&record));
            }
        }
        Ok(SessionSubscription {
            head_seq,
            detail,
            backlog,
            raw,
            updates,
        })
    }

    /// `settings.get`.
    pub fn settings(&self) -> DaemonSettings {
        self.inner.with_core(|core| core.settings.clone())
    }

    /// Raises a blocking request on the session for the daemon itself (e.g. a sandbox
    /// approval) and returns its handle; see [`RaisedRequest`].
    pub async fn raise(
        &self,
        id: SessionId,
        request: RaiseRequest,
    ) -> Result<RaisedRequest, RpcError> {
        self.recovered().await;
        crate::raised::raise(&self.inner, id, request)
    }

    // -----------------------------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------------------------

    /// `session.create`: resolves the agent and the cwd, then creates and starts the session.
    pub async fn create(
        &self,
        params: SessionCreateParams,
    ) -> Result<SessionCreateResult, RpcError> {
        const METHOD: &str = "session.create";
        self.recovered().await;
        let hash = params_hash(&params);
        let stored = self
            .inner
            .with_core(|core| core.stored_op(METHOD, &params.op_id, &hash))?;
        if let Some(result) = stored {
            return Ok(result);
        }
        let driver = self.inner.agents.resolve(&params.agent)?;
        let cwd = self.inner.host.resolve_cwd(&params.cwd).await?;
        self.command(METHOD, &params.op_id, &params, |core| {
            core.create(&params, driver, cwd)
        })
        .await
    }

    /// `session.prompt`.
    pub async fn prompt(&self, params: SessionPromptParams) -> Result<PromptResult, RpcError> {
        self.command("session.prompt", &params.op_id, &params, |core| {
            core.prompt(&params)
        })
        .await
    }

    /// `session.cancel`.
    pub async fn cancel(&self, params: SessionOpParams) -> Result<(), RpcError> {
        self.command("session.cancel", &params.op_id, &params, |core| {
            core.cancel(&params)
        })
        .await
    }

    /// `session.close`.
    pub async fn close(&self, params: SessionOpParams) -> Result<(), RpcError> {
        self.command("session.close", &params.op_id, &params, |core| {
            core.close(&params)
        })
        .await
    }

    /// `session.reopen` (`deleted` when retention purged it).
    pub async fn reopen(&self, params: SessionOpParams) -> Result<(), RpcError> {
        self.command("session.reopen", &params.op_id, &params, |core| {
            core.reopen(&params)
        })
        .await
    }

    /// `session.delete`.
    pub async fn delete(&self, params: SessionOpParams) -> Result<(), RpcError> {
        self.command("session.delete", &params.op_id, &params, |core| {
            core.delete(params.id)
        })
        .await
    }

    /// `session.resume`.
    pub async fn resume(&self, params: SessionResumeParams) -> Result<(), RpcError> {
        self.command("session.resume", &params.op_id, &params, |core| {
            core.resume(params.id, params.sandbox.as_ref())
        })
        .await
    }

    /// `session.start_fresh`: exports the history first when asked to.
    pub async fn start_fresh(
        &self,
        params: SessionStartFreshParams,
    ) -> Result<StartFreshResult, RpcError> {
        const METHOD: &str = "session.start_fresh";
        self.recovered().await;
        let hash = params_hash(&params);
        let stored = self
            .inner
            .with_core(|core| core.stored_op(METHOD, &params.op_id, &hash))?;
        if let Some(result) = stored {
            return Ok(result);
        }
        let id = params.id;
        let history_path = if params.include_history {
            let (items, n) = self.inner.transact(|core| {
                core.flush_timeline(id)?;
                let items = core.store.timeline_since(id, Seq(0)).map_err(internal)?;
                Ok((items, core.session(id)?.data.agent_sessions.len().max(1)))
            })?;
            let markdown = crate::history::export(&items);
            let path = self
                .inner
                .host
                .write_file(id, format!("history-{n}.md"), markdown)
                .await
                .map_err(internal)?;
            Some(path)
        } else {
            None
        };
        self.command(METHOD, &params.op_id, &params, |core| {
            core.start_fresh(
                id,
                &params.op_id,
                params.sandbox.as_ref(),
                history_path.clone(),
            )?;
            Ok(StartFreshResult { history_path })
        })
        .await
    }

    /// `session.update`.
    pub async fn update(&self, params: SessionUpdateParams) -> Result<(), RpcError> {
        self.command("session.update", &params.op_id, &params, |core| {
            core.update(&params)
        })
        .await
    }

    /// `session.mark_read`.
    pub async fn mark_read(&self, params: SessionMarkReadParams) -> Result<ReadCursor, RpcError> {
        self.command("session.mark_read", &params.op_id, &params, |core| {
            core.mark_read(&params)
        })
        .await
    }

    /// `session.mark_unread`.
    pub async fn mark_unread(&self, params: SessionOpParams) -> Result<ReadCursor, RpcError> {
        self.command("session.mark_unread", &params.op_id, &params, |core| {
            core.mark_unread(params.id)
        })
        .await
    }

    /// `session.set_option`.
    pub async fn set_option(&self, params: SessionSetOptionParams) -> Result<(), RpcError> {
        self.command("session.set_option", &params.op_id, &params, |core| {
            core.set_option(&params)
        })
        .await
    }

    /// `queue.send_now`.
    pub async fn send_now(&self, params: QueueSendNowParams) -> Result<SendNowResult, RpcError> {
        self.command("queue.send_now", &params.op_id, &params, |core| {
            core.send_now(&params)
        })
        .await
    }

    /// `queue.edit`.
    pub async fn queue_edit(&self, params: QueueEditParams) -> Result<(), RpcError> {
        self.command("queue.edit", &params.op_id, &params, |core| {
            core.queue_edit(&params)
        })
        .await
    }

    /// `queue.remove`.
    pub async fn queue_remove(&self, params: QueueRemoveParams) -> Result<(), RpcError> {
        self.command("queue.remove", &params.op_id, &params, |core| {
            core.queue_remove(&params)
        })
        .await
    }

    /// `queue.reorder`.
    pub async fn queue_reorder(&self, params: QueueReorderParams) -> Result<(), RpcError> {
        self.command("queue.reorder", &params.op_id, &params, |core| {
            core.queue_reorder(&params)
        })
        .await
    }

    /// `queue.resume`.
    pub async fn queue_resume(&self, params: SessionOpParams) -> Result<(), RpcError> {
        self.command("queue.resume", &params.op_id, &params, |core| {
            core.queue_resume(params.id)
        })
        .await
    }

    /// `request.respond`.
    pub async fn respond(
        &self,
        params: RequestRespondParams,
    ) -> Result<RequestRespondResult, RpcError> {
        self.command("request.respond", &params.op_id, &params, |core| {
            core.respond(&params)
        })
        .await
    }

    /// `settings.set`.
    pub async fn set_settings(
        &self,
        params: SettingsSetParams,
    ) -> Result<DaemonSettings, RpcError> {
        self.command("settings.set", &params.op_id, &params, |core| {
            let settings = core.settings_mut();
            if let Some(hours) = params.idle_timeout_h {
                settings.idle_timeout_h = hours;
            }
            if let Some(retention) = &params.retention {
                settings.retention = retention.clone();
            }
            Ok(settings.clone())
        })
        .await
    }

    #[cfg(test)]
    pub(crate) fn inner(&self) -> &Arc<Inner> {
        &self.inner
    }
}

impl From<MigrateError> for OpenError {
    fn from(e: MigrateError) -> Self {
        OpenError::Database(e.to_string())
    }
}
