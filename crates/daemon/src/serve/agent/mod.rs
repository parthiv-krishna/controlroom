//! The agent sockets (spec §9, `docs/protocol.md` §9): one unix socket per session,
//! `run/<id>.agent.sock`, served for `@controlroom/pi` running inside that session's agent.
//!
//! - **Isolation.** A session's sandbox profile grants only its own socket, and a socket
//!   serves only its own session: the agent can't name another session anywhere in the API.
//! - **Authentication.** `hello` carries the launch's token (`CONTROLROOM_AGENT_TOKEN`, from
//!   the env file), compared in constant time against `sessions/<id>/agent.token`. A token
//!   rotates with every launch, so an older launch's agent can't come back.
//! - **Lifetime.** A socket is bound before its agent starts (from the launch) and again for
//!   every open session when the daemon starts, so agents that outlive the daemon reconnect.
//!   Deleting the session stops its listener and removes the socket.

mod conn;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use controlroom_protocol::ids::SessionId;
use controlroom_runtime::Runtime;
use tokio::net::UnixListener;
use tokio::sync::Semaphore;
use tokio::task::AbortHandle;

use crate::dirs::{Dirs, create_private_dir};

/// Connections served at once per session (pi reloads reconnect; nothing needs more).
const MAX_CONNECTIONS: usize = 8;

/// The agent sockets of this daemon.
pub struct AgentSockets {
    dirs: Dirs,
    runtime: OnceLock<Runtime>,
    handle: tokio::runtime::Handle,
    listeners: Mutex<HashMap<SessionId, AbortHandle>>,
}

impl AgentSockets {
    /// Sockets under `dirs`, served on `handle`. Nothing is bound until [`AgentSockets::serve`].
    pub fn new(dirs: Dirs, handle: tokio::runtime::Handle) -> Arc<Self> {
        Arc::new(Self {
            dirs,
            runtime: OnceLock::new(),
            handle,
            listeners: Mutex::new(HashMap::new()),
        })
    }

    /// Starts serving with `runtime`, binding the socket of every session in `sessions`.
    pub fn serve(self: &Arc<Self>, runtime: Runtime, sessions: impl IntoIterator<Item = SessionId>) {
        if self.runtime.set(runtime).is_err() {
            return;
        }
        for id in sessions {
            if let Err(e) = self.ensure(id) {
                tracing::warn!("session {id}: binding its agent socket: {e}");
            }
        }
    }

    /// Binds the session's socket unless it is served already. Blocking-safe: it only binds
    /// and spawns. Before [`AgentSockets::serve`] it does nothing.
    pub fn ensure(self: &Arc<Self>, id: SessionId) -> std::io::Result<()> {
        let Some(runtime) = self.runtime.get().cloned() else {
            return Ok(());
        };
        let mut listeners = self.lock();
        if listeners.get(&id).is_some_and(|task| !task.is_finished()) {
            return Ok(());
        }
        let path = self.dirs.agent_socket(&id.to_string());
        create_private_dir(&self.dirs.run())?;
        match std::fs::remove_file(path.as_path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let listener = std::os::unix::net::UnixListener::bind(path.as_path())?;
        listener.set_nonblocking(true)?;
        let _guard = self.handle.enter();
        let listener = UnixListener::from_std(listener)?;
        let dirs = self.dirs.clone();
        let task = self.handle.spawn(accept(listener, runtime, dirs, id));
        listeners.insert(id, task.abort_handle());
        Ok(())
    }

    /// Stops serving the session and removes its socket (the session was deleted).
    pub fn forget(&self, id: SessionId) {
        if let Some(task) = self.lock().remove(&id) {
            task.abort();
        }
        let path = self.dirs.agent_socket(&id.to_string());
        if let Err(e) = std::fs::remove_file(path.as_path())
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("removing {path}: {e}");
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, AbortHandle>> {
        self.listeners.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Accepts the session's agent connections.
async fn accept(listener: UnixListener, runtime: Runtime, dirs: Dirs, id: SessionId) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let Ok(permit) = slots.clone().acquire_owned().await else {
            return;
        };
        match listener.accept().await {
            Ok((stream, _)) => {
                let (runtime, dirs) = (runtime.clone(), dirs.clone());
                tokio::spawn(async move {
                    conn::serve(stream, runtime, dirs, id).await;
                    drop(permit);
                });
            }
            Err(e) => {
                tracing::warn!("session {id}: agent socket accept: {e}");
                return;
            }
        }
    }
}
