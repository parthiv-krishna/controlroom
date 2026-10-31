//! Establishing an authenticated russh connection with host-key pinning.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, DisconnectReason, Handle, Msg};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{Channel, ChannelStream, Preferred};
use tokio::sync::{oneshot, watch};

use super::error::SshError;
use super::exec::{CommandOutput, ExecChannel};
use super::host_key::{HostKey, HostKeyCheck, check_host_key};
use super::target::SshTarget;
use crate::keys::SshPrivateKey;

/// Connection tuning. The defaults suit an always-on server reaching hosts
/// over a LAN or tailnet.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// Covers TCP connect, key exchange and authentication.
    pub connect_timeout: Duration,
    /// Send a keepalive after this long without hearing from the server.
    pub keepalive_interval: Duration,
    /// Drop the connection after this many unanswered keepalives.
    pub keepalive_max: usize,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(15),
            keepalive_interval: Duration::from_secs(15),
            keepalive_max: 3,
        }
    }
}

/// Errors the russh handler raises to abort the handshake.
#[derive(Debug)]
enum HandlerError {
    Russh(russh::Error),
    HostKey(HostKeyCheck),
}

impl From<russh::Error> for HandlerError {
    fn from(e: russh::Error) -> Self {
        HandlerError::Russh(e)
    }
}

struct ClientHandler {
    pinned: Option<HostKey>,
    /// Holds the last disconnect reason; dropped with the session task, which
    /// wakes [`SshConnection::closed`].
    closed_tx: watch::Sender<Option<String>>,
    /// Receives the presented key, for [`fetch_host_key`].
    presented_tx: Option<oneshot::Sender<HostKey>>,
}

impl client::Handler for ClientHandler {
    type Error = HandlerError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            // Host certificates are not supported; pin the certified key.
            PublicKeyOrCertificate::Certificate(cert) => {
                russh::keys::PublicKey::new(cert.public_key().clone(), "")
            }
        };
        if let Some(tx) = self.presented_tx.take() {
            let _ = tx.send(HostKey::from_public_key(&key));
        }
        match check_host_key(self.pinned.as_ref(), &key) {
            HostKeyCheck::Trusted => Ok(true),
            other => Err(HandlerError::HostKey(other)),
        }
    }

    async fn disconnected(
        &mut self,
        reason: DisconnectReason<Self::Error>,
    ) -> Result<(), Self::Error> {
        let text = match &reason {
            DisconnectReason::ReceivedDisconnect(info) => {
                format!("server disconnected: {}", info.message)
            }
            DisconnectReason::Error(HandlerError::Russh(e)) => e.to_string(),
            DisconnectReason::Error(HandlerError::HostKey(check)) => {
                format!("host key rejected: {check:?}")
            }
        };
        self.closed_tx.send_replace(Some(text));
        match reason {
            DisconnectReason::ReceivedDisconnect(_) => Ok(()),
            DisconnectReason::Error(e) => Err(e),
        }
    }
}

fn map_handler_error(e: HandlerError) -> SshError {
    match e {
        HandlerError::HostKey(HostKeyCheck::Unknown(host_key)) => {
            SshError::UnknownHostKey { host_key }
        }
        HandlerError::HostKey(HostKeyCheck::Mismatch { expected, actual }) => {
            SshError::HostKeyMismatch { expected, actual }
        }
        HandlerError::HostKey(HostKeyCheck::Trusted) => {
            SshError::Other("host key check failed".into())
        }
        HandlerError::Russh(e) => e.into(),
    }
}

/// An authenticated SSH connection. Channels are multiplexed: open as many
/// exec, session or direct-tcpip channels as needed, concurrently.
pub struct SshConnection {
    handle: Handle<ClientHandler>,
    host_key: HostKey,
    closed_rx: watch::Receiver<Option<String>>,
}

impl std::fmt::Debug for SshConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshConnection")
            .field("host_key", &self.host_key)
            .field("closed", &self.is_closed())
            .finish()
    }
}

/// Connects and authenticates with `key`.
///
/// `pinned_host_key: None` is first contact: the handshake stops after key
/// exchange and returns [`SshError::UnknownHostKey`] with the server's key, so
/// the user can confirm its fingerprint. Once confirmed, the caller stores the
/// key and connects again with it pinned; a different key is then
/// [`SshError::HostKeyMismatch`]. With a pin, only the pinned key's algorithm
/// is offered, so a host with several host keys presents the pinned one.
pub async fn connect(
    target: &SshTarget,
    key: &SshPrivateKey,
    pinned_host_key: Option<&HostKey>,
    options: &ConnectOptions,
) -> Result<SshConnection, SshError> {
    with_timeout(
        options.connect_timeout,
        connect_inner(target, key, pinned_host_key, options),
    )
    .await
}

/// Retrieves the server's host key without authenticating (first contact).
pub async fn fetch_host_key(
    target: &SshTarget,
    options: &ConnectOptions,
) -> Result<HostKey, SshError> {
    let fetch = async {
        let stream = tcp_connect(target).await?;
        let (tx, rx) = oneshot::channel();
        let (closed_tx, _) = watch::channel(None);
        let handler = ClientHandler {
            pinned: None,
            closed_tx,
            presented_tx: Some(tx),
        };
        let config = Arc::new(client_config(options, None));
        let result = client::connect_stream(config, stream, handler).await;
        match (result, rx.await) {
            (_, Ok(key)) => Ok(key),
            (Err(e), Err(_)) => Err(map_handler_error(e)),
            (Ok(_), Err(_)) => Err(SshError::Other(
                "server completed key exchange without presenting a host key".into(),
            )),
        }
    };
    with_timeout(options.connect_timeout, fetch).await
}

async fn with_timeout<T>(
    after: Duration,
    fut: impl Future<Output = Result<T, SshError>>,
) -> Result<T, SshError> {
    tokio::time::timeout(after, fut)
        .await
        .unwrap_or(Err(SshError::Timeout { after }))
}

fn client_config(options: &ConnectOptions, pinned: Option<&HostKey>) -> client::Config {
    let mut preferred = Preferred::default();
    if let Some(pinned) = pinned {
        preferred.key = Cow::Owned(pinned.preferred_algorithms(&preferred.key));
    }
    client::Config {
        keepalive_interval: Some(options.keepalive_interval),
        keepalive_max: options.keepalive_max,
        inactivity_timeout: None,
        nodelay: true,
        preferred,
        ..Default::default()
    }
}

async fn tcp_connect(target: &SshTarget) -> Result<tokio::net::TcpStream, SshError> {
    let stream = tokio::net::TcpStream::connect((target.connect_host(), target.port))
        .await
        .map_err(|source| SshError::Unreachable {
            addr: target.addr(),
            source,
        })?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

async fn connect_inner(
    target: &SshTarget,
    key: &SshPrivateKey,
    pinned_host_key: Option<&HostKey>,
    options: &ConnectOptions,
) -> Result<SshConnection, SshError> {
    let stream = tcp_connect(target).await?;
    let (closed_tx, closed_rx) = watch::channel(None);
    let handler = ClientHandler {
        pinned: pinned_host_key.cloned(),
        closed_tx,
        presented_tx: None,
    };
    let config = Arc::new(client_config(options, pinned_host_key));
    let mut handle = client::connect_stream(config, stream, handler)
        .await
        .map_err(map_handler_error)?;
    // The handler refuses every key unless one is pinned, so this holds.
    let host_key = pinned_host_key
        .cloned()
        .ok_or_else(|| SshError::Other("authenticated without a pinned host key".into()))?;

    let hash_alg = if key.inner().algorithm().is_rsa() {
        // Outer None: the server sent no server-sig-algs; modern servers
        // reject SHA-1, so prefer SHA-512.
        handle
            .best_supported_rsa_hash()
            .await?
            .unwrap_or(Some(HashAlg::Sha512))
    } else {
        None
    };
    let auth = handle
        .authenticate_publickey(
            target.username.clone(),
            PrivateKeyWithHashAlg::new(key.inner(), hash_alg),
        )
        .await?;
    if !auth.success() {
        if handle.is_closed() {
            let reason = closed_rx.borrow().clone();
            return Err(SshError::Disconnected(reason.unwrap_or_else(|| {
                "connection closed during authentication".into()
            })));
        }
        return Err(SshError::AuthFailed {
            username: target.username.clone(),
        });
    }
    Ok(SshConnection {
        handle,
        host_key,
        closed_rx,
    })
}

impl SshConnection {
    /// The (pinned) host key this connection verified.
    pub fn host_key(&self) -> &HostKey {
        &self.host_key
    }

    /// Opens a session channel and runs `command` through the user's login
    /// shell, as sshd does (see [`crate::shell::sh_c`] for POSIX scripts).
    ///
    /// Each call opens a new channel multiplexed on this connection, e.g. the
    /// daemon's JSON-RPC bridge, an upload, or a terminal attach.
    pub async fn exec(&self, command: &str) -> Result<ExecChannel, SshError> {
        let channel = self.open_session_channel().await?;
        ExecChannel::start(channel, command)
            .await
            .map_err(|e| self.channel_error(e))
    }

    /// Opens a raw session channel for uses other than exec, such as an SFTP
    /// subsystem on the same connection.
    pub async fn open_session_channel(&self) -> Result<Channel<Msg>, SshError> {
        self.handle
            .channel_open_session()
            .await
            .map_err(|e| self.channel_error(e))
    }

    /// Opens a direct-tcpip channel to `host:port` as seen from the remote
    /// machine (e.g. a dev server on its loopback), as a byte stream.
    pub async fn open_direct_tcpip(
        &self,
        host: &str,
        port: u16,
    ) -> Result<ChannelStream<Msg>, SshError> {
        self.handle
            .channel_open_direct_tcpip(host, port.into(), "127.0.0.1", 0)
            .await
            .map(Channel::into_stream)
            .map_err(|e| self.channel_error(e))
    }

    /// Runs `command` to completion, feeding it `stdin`.
    pub async fn run(&self, command: &str, stdin: &[u8]) -> Result<CommandOutput, SshError> {
        let exec = self.exec(command).await?;
        exec.collect(stdin)
            .await
            .map_err(|e| SshError::Other(format!("exec {command:?}: {e}")))
    }

    /// Whether the connection is gone.
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed() || self.closed_rx.has_changed().is_err()
    }

    /// Resolves when the connection is gone (keepalive timeout, server
    /// disconnect, network error), with a description of why.
    pub async fn closed(&self) -> String {
        let mut rx = self.closed_rx.clone();
        // The sender lives in the session task's handler; it is dropped (and
        // `changed` errors) when the session ends.
        while rx.changed().await.is_ok() {}
        let reason = rx.borrow().clone();
        reason.unwrap_or_else(|| "connection closed".into())
    }

    /// Closes the connection politely.
    pub async fn disconnect(&self) {
        let _ = self
            .handle
            .disconnect(russh::Disconnect::ByApplication, "bye", "en")
            .await;
    }

    fn channel_error(&self, e: russh::Error) -> SshError {
        if self.is_closed() {
            let reason = self.closed_rx.borrow().clone();
            SshError::Disconnected(reason.unwrap_or_else(|| e.to_string()))
        } else {
            SshError::from(e)
        }
    }
}
