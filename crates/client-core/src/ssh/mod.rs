//! SSH connections on in-process russh: host-key pinning, typed errors, and
//! multiplexed channels (exec streams, raw session channels, direct-tcpip).
//!
//! Reconnect policy is not here: the controller's machine manager owns it,
//! using [`SshErrorKind::is_retryable`] and [`SshConnection::closed`].

mod connection;
mod error;
mod exec;
mod host_key;
mod target;

pub use connection::{ConnectOptions, SshConnection, connect, fetch_host_key};
pub use error::{SshError, SshErrorKind};
pub use exec::{
    CommandOutput, ExecChannel, ExecReader, ExecWriter, ExitStatus, ExitWatcher, STDERR_LIMIT,
};
pub use host_key::{HostKey, HostKeyCheck, HostKeyParseError, check_host_key};
pub use target::SshTarget;
