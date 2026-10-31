//! Typed SSH errors, shaped for the UI's connectivity states and recovery flows.

use std::time::Duration;

use super::host_key::HostKey;

/// Why connecting, or using a connection, failed.
#[derive(Debug, thiserror::Error)]
pub enum SshError {
    /// TCP connect or DNS resolution failed.
    #[error("cannot reach {addr}: {source}")]
    Unreachable {
        /// `host:port` as attempted.
        addr: String,
        /// The resolver or socket error.
        #[source]
        source: std::io::Error,
    },
    /// The connect timeout (TCP, key exchange and authentication) expired.
    #[error("timed out connecting after {after:?}")]
    Timeout {
        /// The timeout that expired.
        after: Duration,
    },
    /// First contact: no host key is pinned. No session was established; show
    /// `host_key.fingerprint()` to the user and pin the key once confirmed.
    #[error("unknown host key {}; confirm its fingerprint to continue", host_key.fingerprint())]
    UnknownHostKey {
        /// The key the server presented.
        host_key: HostKey,
    },
    /// The server presented a different key than the pinned one. Hard error.
    #[error(
        "HOST KEY MISMATCH: expected {}, server presented {}; refusing to connect",
        expected.fingerprint(),
        actual.fingerprint()
    )]
    HostKeyMismatch {
        /// The pinned key.
        expected: HostKey,
        /// The key the server presented.
        actual: HostKey,
    },
    /// Public key authentication was rejected ("permission denied").
    #[error("permission denied (publickey) for user {username}")]
    AuthFailed {
        /// The user that was refused.
        username: String,
    },
    /// The connection is gone (keepalive timeout, server disconnect, network error).
    #[error("connection lost: {0}")]
    Disconnected(String),
    /// Any other protocol or channel failure.
    #[error("ssh: {0}")]
    Other(String),
}

/// A copyable classification of [`SshError`] for connectivity state and retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SshErrorKind {
    /// See [`SshError::Unreachable`].
    Unreachable,
    /// See [`SshError::Timeout`].
    Timeout,
    /// See [`SshError::UnknownHostKey`].
    UnknownHostKey,
    /// See [`SshError::HostKeyMismatch`].
    HostKeyMismatch,
    /// See [`SshError::AuthFailed`].
    AuthFailed,
    /// See [`SshError::Disconnected`].
    Disconnected,
    /// See [`SshError::Other`].
    Other,
}

impl SshError {
    /// This error's classification.
    pub fn kind(&self) -> SshErrorKind {
        match self {
            SshError::Unreachable { .. } => SshErrorKind::Unreachable,
            SshError::Timeout { .. } => SshErrorKind::Timeout,
            SshError::UnknownHostKey { .. } => SshErrorKind::UnknownHostKey,
            SshError::HostKeyMismatch { .. } => SshErrorKind::HostKeyMismatch,
            SshError::AuthFailed { .. } => SshErrorKind::AuthFailed,
            SshError::Disconnected(_) => SshErrorKind::Disconnected,
            SshError::Other(_) => SshErrorKind::Other,
        }
    }
}

impl SshErrorKind {
    /// Whether retrying without user action can help. Host key problems and
    /// rejected authentication need the user (confirm the key, install the
    /// public key), so a reconnect loop stops on them.
    pub fn is_retryable(self) -> bool {
        !matches!(
            self,
            SshErrorKind::UnknownHostKey | SshErrorKind::HostKeyMismatch | SshErrorKind::AuthFailed
        )
    }
}

impl From<russh::Error> for SshError {
    fn from(e: russh::Error) -> Self {
        match e {
            russh::Error::ConnectionTimeout
            | russh::Error::KeepaliveTimeout
            | russh::Error::InactivityTimeout
            | russh::Error::Disconnect
            | russh::Error::HUP => SshError::Disconnected(e.to_string()),
            other => SshError::Other(other.to_string()),
        }
    }
}
