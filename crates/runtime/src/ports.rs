//! The IO the runtime needs, as traits the daemon implements (in-memory fakes in the tests).
//!
//! - [`Host`]: supervisors and session files on this machine;
//! - [`SupervisorConn`] and [`RecordStream`]: one writer connection to a running supervisor;
//! - [`Agents`]: the configured agents, their drivers and the spawn spec of a launch (sandbox
//!   profile preparation happens behind it);
//! - [`Clock`]: the time.

use std::sync::Arc;
use std::time::Duration;

use controlroom_drivers::{Driver, StartMode};
use controlroom_protocol::RpcError;
use controlroom_protocol::ids::{RemotePath, Seq, SessionId, UnixMillis};
use controlroom_protocol::session::{DriverKind, SandboxLevel, SandboxSpec};
use controlroom_supervisor::SpawnSpec;
use controlroom_transcript::Record;
use futures::future::BoxFuture;

/// A failed host operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HostError(pub String);

/// Supervisors, transcripts and session files on this machine.
pub trait Host: Send + Sync + 'static {
    /// Expands `~` in a working directory and checks it is a directory (`fs_error` if not).
    fn resolve_cwd(&self, cwd: &RemotePath) -> BoxFuture<'_, Result<RemotePath, RpcError>>;

    /// Starts a detached supervisor for the session's agent and returns once it is ready.
    fn launch(&self, session: SessionId, spec: SpawnSpec) -> BoxFuture<'_, Result<(), HostError>>;

    /// Connects to the session's running supervisor in the single writer role. `None` when no
    /// supervisor runs. A previous daemon's writer connection may take a moment to be released;
    /// the implementation retries while the role is busy.
    fn connect(
        &self,
        session: SessionId,
    ) -> BoxFuture<'_, Result<Option<Arc<dyn SupervisorConn>>, HostError>>;

    /// Reads the session's transcript from `from` to its current end (no supervisor needed).
    fn read_transcript(
        &self,
        session: SessionId,
        from: Seq,
    ) -> BoxFuture<'_, Result<Box<dyn RecordStream>, HostError>>;

    /// Writes a file for the session's agent to read (history exports) where its sandbox lets it
    /// read, and returns its absolute path.
    fn write_file(
        &self,
        session: SessionId,
        name: String,
        contents: String,
    ) -> BoxFuture<'_, Result<RemotePath, HostError>>;

    /// Removes everything the session left on disk (transcript, directories).
    fn remove(&self, session: SessionId) -> BoxFuture<'_, Result<(), HostError>>;
}

/// A writer connection to one supervisor. Dropping it releases the writer role.
pub trait SupervisorConn: Send + Sync + 'static {
    /// Logs and fsyncs `line`, then hands it to the agent; returns its seq.
    fn write(&self, line: String) -> BoxFuture<'_, Result<Seq, HostError>>;

    /// Waits until every write accepted before it (from any connection) is logged and fsynced,
    /// and returns the durable seq.
    fn barrier(&self) -> BoxFuture<'_, Result<Seq, HostError>>;

    /// Streams records from `from`, then live ones; the stream ends after the `exit` record or
    /// when the supervisor goes away.
    fn read(&self, from: Seq) -> BoxFuture<'_, Result<Box<dyn RecordStream>, HostError>>;

    /// Stops the agent: SIGTERM to its process group, SIGKILL after `grace`.
    fn terminate(&self, grace: Duration) -> BoxFuture<'_, Result<(), HostError>>;
}

/// Transcript records in seq order.
pub trait RecordStream: Send {
    /// The next record; `None` at the end.
    fn next(&mut self) -> BoxFuture<'_, Option<Record>>;
}

/// Everything a launch needs to become a spawn spec.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentLaunch {
    /// The session (also the supervised process id).
    pub session: SessionId,
    /// The agent key.
    pub agent: String,
    /// The working directory.
    pub cwd: RemotePath,
    /// New or resumed native session.
    pub mode: StartMode,
    /// The driver's arguments for this start, appended to the agent command.
    pub args: Vec<String>,
    /// The level the agent runs at (explicit or default).
    pub sandbox_level: SandboxLevel,
    /// The session's sandbox defaults (grants, domains, mediated commands).
    pub sandbox: SandboxSpec,
}

/// The machine's configured agents.
pub trait Agents: Send + Sync + 'static {
    /// The driver kind of an agent key: `not_found` (agent) for an unknown key,
    /// `agent_unavailable` when its dependency check fails.
    fn resolve(&self, agent: &str) -> Result<DriverKind, RpcError>;

    /// A fresh driver of this kind (sessions remember their kind, so old transcripts are
    /// folded by the driver that wrote them).
    fn driver(&self, kind: DriverKind) -> Box<dyn Driver>;

    /// Prepares the sandbox and returns what the supervisor runs; `sandbox_unavailable` when
    /// the sandbox can't be set up. May block on file IO.
    fn spawn_spec(&self, launch: &AgentLaunch) -> Result<SpawnSpec, RpcError>;
}

/// The time.
pub trait Clock: Send + Sync + 'static {
    /// Now.
    fn now(&self) -> UnixMillis;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UnixMillis {
        UnixMillis::now()
    }
}
