//! The synchronous half of the agent process lifecycle: what happens under the lock when a
//! launch starts or fails, a supervisor is attached or lost, a barrier returns, or a write
//! fails. The async half (talking to the host) is in `io`.

use std::sync::Arc;

use controlroom_drivers::StartMode;
use controlroom_protocol::RpcError;
use controlroom_protocol::ids::{Seq, SessionId};
use controlroom_protocol::session::StartedBy;
use tokio::sync::mpsc;

use crate::core::{Core, Task};
use crate::errors::internal;
use crate::ports::{AgentLaunch, SupervisorConn};
use crate::session::{Attachment, Doubt, Session};

/// What a new attachment reads from.
pub(crate) struct Attached {
    pub generation: u64,
    pub from: Seq,
}

impl Core {
    /// The spawn inputs for the session's pending start, or `None` when it shouldn't start
    /// (closed, deleted, or already running).
    pub(crate) fn prepare_launch(
        &mut self,
        id: SessionId,
    ) -> Result<Option<AgentLaunch>, RpcError> {
        let Some(session) = self.sessions.get(&id) else {
            return Ok(None);
        };
        let running = session.live.attachment.is_some() && session.derived.process.live;
        if session.data.closed() || running {
            self.session_mut(id)?.live.launching = false;
            return Ok(None);
        }
        let data = &session.data;
        let mode = match (data.pending_start, data.agent_sessions.last()) {
            (Some(StartedBy::Resume), Some(row)) => StartMode::Resume {
                native: row.native(),
            },
            _ => StartMode::New {
                native_id: SessionId::generate().to_string(),
            },
        };
        Ok(Some(AgentLaunch {
            session: id,
            args: session.driver().launch_args(&mode),
            mode,
            sandbox_level: data.sandbox_level(),
            sandbox: data.sandbox.clone(),
        }))
    }

    pub(crate) fn launch_failed(&mut self, id: SessionId, error: String) -> Result<(), RpcError> {
        tracing::warn!("session {id}: launch failed: {error}");
        let session = self.session_mut(id)?;
        session.live.launching = false;
        session.live.launch_error = Some(error);
        Ok(())
    }

    /// Takes over a supervisor connection. The session is in doubt until the barrier's durable
    /// seq has been folded. `None` when the session is gone (the caller stops the agent).
    pub(crate) fn attach(
        &mut self,
        id: SessionId,
        conn: Arc<dyn SupervisorConn>,
        writer: mpsc::Sender<String>,
    ) -> Result<Option<Attached>, RpcError> {
        if !self.sessions.contains_key(&id) {
            return Ok(None);
        }
        let generation = self.next_generation();
        let session = self.session_mut(id)?;
        session.abort_tasks();
        session.live.attachment = Some(Attachment {
            generation,
            writer,
            conn: conn.clone(),
        });
        session.live.doubt = Some(Doubt {
            until: None,
            write_failed: false,
        });
        session.live.launching = false;
        session.live.loading = false;
        let from = Seq(session.live.processed_seq.0 + 1);
        if session.data.closed() {
            self.task(Task::Terminate(conn, std::time::Duration::ZERO));
        }
        Ok(Some(Attached { generation, from }))
    }

    /// The barrier answered: writes resume once the transcript is folded through `durable`.
    pub(crate) fn barrier_done(
        &mut self,
        id: SessionId,
        generation: u64,
        durable: Seq,
    ) -> Result<(), RpcError> {
        if !self.is_current(id, generation) {
            return Ok(());
        }
        let session = self.session_mut(id)?;
        if let Some(doubt) = &mut session.live.doubt {
            doubt.until = Some(durable);
        }
        self.resolve_doubt(id);
        self.reconcile(id);
        Ok(())
    }

    /// A write failed: stop writing and find out, with a barrier, what reached the transcript.
    pub(crate) fn write_failed(&mut self, id: SessionId, generation: u64) -> Result<(), RpcError> {
        if !self.is_current(id, generation) {
            return Ok(());
        }
        let session = self.session_mut(id)?;
        if session.live.doubt.is_some() {
            return Ok(());
        }
        session.live.doubt = Some(Doubt {
            until: None,
            write_failed: true,
        });
        if let Some(attachment) = &session.live.attachment {
            let conn = attachment.conn.clone();
            self.task(Task::Barrier {
                id,
                generation,
                conn,
            });
        }
        Ok(())
    }

    /// The supervisor's stream ended and the transcript was read to its end: the process is
    /// gone. Starts it again when a restart was asked for.
    pub(crate) fn detach(&mut self, id: SessionId, generation: u64) -> Result<(), RpcError> {
        if !self.is_current(id, generation) {
            return Ok(());
        }
        let session = self.session_mut(id)?;
        session.live.attachment = None;
        session.live.stopping = false;
        session.abort_tasks();
        self.after_process(id)
    }

    /// Recovery found no supervisor for the session (a reboot, or it died while the daemon was
    /// down).
    pub(crate) fn no_supervisor(&mut self, id: SessionId) -> Result<(), RpcError> {
        let session = self.session_mut(id)?;
        session.live.loading = false;
        let unfinished_start = session.data.pending_start.is_some();
        if unfinished_start && !session.data.closed() {
            session.live.relaunch = true;
        }
        self.after_process(id)
    }

    fn after_process(&mut self, id: SessionId) -> Result<(), RpcError> {
        self.agent_gone(id)?;
        self.release_done_calls(id);
        self.settle_handed(id, false);
        let session = self.session_mut(id)?;
        if std::mem::take(&mut session.live.relaunch) && !session.data.closed() {
            let by = session.data.pending_start.unwrap_or(StartedBy::Fresh);
            self.request_start(id, by)?;
        }
        Ok(())
    }

    /// Replaces the session with its committed state, to be re-folded (after a failed
    /// transaction left its driver ahead of the store).
    pub(crate) fn reset_session(&mut self, id: SessionId) -> Result<bool, RpcError> {
        let Some(data) = self.store.load_session(id).map_err(internal)? else {
            return Ok(false);
        };
        let Some(old) = self.sessions.get_mut(&id) else {
            return Ok(false);
        };
        old.abort_tasks();
        let updates = old.updates.clone();
        let agent = old.agent.clone();
        let emitted = old.emitted.clone();
        let old_info = old.info.clone();
        let old_events = old.events.clone();
        let driver = self.agents.driver(data.driver);
        let mut session = Session::new(data, driver, self.machines.clone());
        session.updates = updates;
        session.agent = agent;
        session.info = old_info;
        session.events = old_events;
        session.emitted = emitted;
        session.live.loading = true;
        self.sessions.insert(id, session);
        Ok(true)
    }

    fn is_current(&self, id: SessionId, generation: u64) -> bool {
        self.sessions
            .get(&id)
            .and_then(|s| s.live.attachment.as_ref())
            .is_some_and(|a| a.generation == generation)
    }
}
