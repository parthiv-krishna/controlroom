//! The runtime's [`Host`] port on the server: detached supervisors over their sockets,
//! transcripts and session files under the state dir.
//!
//! Each [`SupervisorConn`] holds one writer connection (`write`, `barrier`, `terminate`) and
//! opens a separate reader connection per `read`, so a record stream that isn't drained never
//! stalls a barrier on the writer connection.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use controlroom_protocol::ids::{RemotePath, Seq, SessionId};
use controlroom_runtime::ports::{Host, HostError, RecordStream, SupervisorConn};
use controlroom_sandbox::SessionLayout;
use controlroom_supervisor::{Client, ClientError, ErrorCode, LaunchRequest, SpawnSpec};
use controlroom_transcript::{Reader, Record};
use futures::future::BoxFuture;
use tokio::sync::mpsc;

use super::agent::AgentSockets;
use crate::dirs::Dirs;
use crate::launch::supervisor_paths;

/// How long `connect` keeps retrying while a previous writer connection is released.
const WRITER_RETRY: Duration = Duration::from_secs(10);

/// Records read ahead from a transcript file.
const FILE_READ_AHEAD: usize = 256;

/// Supervisors and session files of the agent daemon.
pub struct ServerHost {
    dirs: Dirs,
    /// The `controlroom-agentd` that runs supervisors.
    agentd: PathBuf,
    /// The agent sockets (a deleted session's is stopped and removed).
    sockets: Arc<AgentSockets>,
}

impl ServerHost {
    /// A host for `dirs`, running supervisors from `agentd`.
    pub fn new(dirs: Dirs, agentd: PathBuf, sockets: Arc<AgentSockets>) -> Self {
        Self {
            dirs,
            agentd,
            sockets,
        }
    }

    fn session_id(id: SessionId) -> Result<controlroom_sandbox::SessionId, HostError> {
        controlroom_sandbox::SessionId::new(&id.to_string()).map_err(|e| HostError(e.to_string()))
    }
}

fn host_error(e: impl std::fmt::Display) -> HostError {
    HostError(e.to_string())
}

impl Host for ServerHost {
    fn launch(&self, session: SessionId, spec: SpawnSpec) -> BoxFuture<'_, Result<(), HostError>> {
        Box::pin(async move {
            let id = Self::session_id(session)?;
            let request = LaunchRequest {
                id: id.as_str().to_owned(),
                paths: supervisor_paths(&self.dirs, &id),
                spec,
            };
            controlroom_supervisor::launch(&self.agentd, &request)
                .await
                .map(|_| ())
                .map_err(host_error)
        })
    }

    fn connect(
        &self,
        session: SessionId,
    ) -> BoxFuture<'_, Result<Option<Arc<dyn SupervisorConn>>, HostError>> {
        Box::pin(async move {
            let id = Self::session_id(session)?;
            connect_writer(supervisor_paths(&self.dirs, &id).socket).await
        })
    }

    fn read_transcript(
        &self,
        session: SessionId,
        from: Seq,
    ) -> BoxFuture<'_, Result<Box<dyn RecordStream>, HostError>> {
        Box::pin(async move {
            let id = Self::session_id(session)?;
            let path = supervisor_paths(&self.dirs, &id)
                .dir
                .join("transcript.jsonl");
            Ok(Box::new(read_file(path, from.0)) as Box<dyn RecordStream>)
        })
    }

    fn write_file(
        &self,
        session: SessionId,
        name: String,
        contents: String,
    ) -> BoxFuture<'_, Result<RemotePath, HostError>> {
        Box::pin(async move {
            if name.is_empty() || name.contains('/') || name.starts_with('.') {
                return Err(HostError(format!("invalid file name {name:?}")));
            }
            let id = Self::session_id(session)?;
            // The agent reads these (history exports), so they go where a sandboxed agent may
            // read: its HOME. The session dir itself is protected from the sandbox.
            let dir = SessionLayout::new(self.dirs.state(), &id)
                .home()
                .as_path()
                .to_owned();
            tokio::fs::create_dir_all(&dir).await.map_err(host_error)?;
            let path = dir.join(&name);
            tokio::fs::write(&path, contents)
                .await
                .map_err(host_error)?;
            Ok(RemotePath::from(path.to_string_lossy().as_ref()))
        })
    }

    fn remove(&self, session: SessionId) -> BoxFuture<'_, Result<(), HostError>> {
        Box::pin(async move {
            self.sockets.forget(session);
            let id = Self::session_id(session)?;
            let paths = supervisor_paths(&self.dirs, &id);
            for result in [
                tokio::fs::remove_dir_all(&paths.dir).await,
                tokio::fs::remove_file(&paths.socket).await,
            ] {
                match result {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                        return Err(host_error(e));
                    }
                    _ => {}
                }
            }
            Ok(())
        })
    }
}

/// Connects to the supervisor at `socket` in the writer role; `None` when none runs. Retries
/// while a previous writer connection is being released.
async fn connect_writer(socket: PathBuf) -> Result<Option<Arc<dyn SupervisorConn>>, HostError> {
    let deadline = Instant::now() + WRITER_RETRY;
    loop {
        match Client::connect(&socket, true).await {
            Ok(client) => {
                let conn: Arc<dyn SupervisorConn> = Arc::new(DaemonConn {
                    writer: client,
                    socket,
                });
                return Ok(Some(conn));
            }
            Err(ClientError::Remote(e))
                if e.code == ErrorCode::WriterBusy && Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(ClientError::Io(e)) if not_running(&e) => return Ok(None),
            // The supervisor exited between accepting and answering.
            Err(ClientError::Disconnected) => return Ok(None),
            Err(e) => return Err(host_error(e)),
        }
    }
}

/// No supervisor listens: the socket is gone or refuses connections.
fn not_running(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

/// A writer connection to one supervisor.
struct DaemonConn {
    writer: Client,
    socket: PathBuf,
}

impl SupervisorConn for DaemonConn {
    fn write(&self, line: String) -> BoxFuture<'_, Result<Seq, HostError>> {
        Box::pin(async move { self.writer.write(&line).await.map(Seq).map_err(host_error) })
    }

    fn barrier(&self) -> BoxFuture<'_, Result<Seq, HostError>> {
        Box::pin(async move { self.writer.barrier().await.map(Seq).map_err(host_error) })
    }

    fn read(&self, from: Seq) -> BoxFuture<'_, Result<Box<dyn RecordStream>, HostError>> {
        Box::pin(async move {
            let reader = Client::connect(&self.socket, false)
                .await
                .map_err(host_error)?;
            let records = reader.read(from.0).await.map_err(host_error)?;
            Ok(Box::new(SupervisorRecords {
                _client: reader,
                records,
            }) as Box<dyn RecordStream>)
        })
    }

    fn terminate(&self, grace: Duration) -> BoxFuture<'_, Result<(), HostError>> {
        Box::pin(async move {
            self.writer
                .terminate(Some(grace))
                .await
                .map(|_| ())
                .map_err(host_error)
        })
    }
}

/// A live record stream on its own reader connection.
struct SupervisorRecords {
    _client: Client,
    records: controlroom_supervisor::RecordStream,
}

impl RecordStream for SupervisorRecords {
    fn next(&mut self) -> BoxFuture<'_, Option<Record>> {
        Box::pin(self.records.next())
    }
}

/// Records of a transcript file, read on a blocking thread.
struct FileRecords(mpsc::Receiver<Record>);

impl RecordStream for FileRecords {
    fn next(&mut self) -> BoxFuture<'_, Option<Record>> {
        Box::pin(self.0.recv())
    }
}

/// Streams the complete records of `path` from `from` to its current end. A missing file is
/// an empty transcript; a corrupt record ends the stream (logged).
fn read_file(path: PathBuf, from: u64) -> FileRecords {
    let (tx, rx) = mpsc::channel(FILE_READ_AHEAD);
    tokio::task::spawn_blocking(move || {
        let mut reader = match Reader::open(&path, from) {
            Ok(reader) => reader,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!("reading {}: {e}", path.display());
                return;
            }
        };
        loop {
            match reader.next_record() {
                Ok(Some(record)) => {
                    if tx.blocking_send(record).is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!("reading {}: {e}", path.display());
                    return;
                }
            }
        }
    });
    FileRecords(rx)
}
