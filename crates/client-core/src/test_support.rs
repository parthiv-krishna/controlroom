//! Test harness (feature `test-support`, Unix only): an in-process russh SSH
//! server whose exec requests run `sh -c <command>` locally with `HOME` set
//! to a temp directory, plus a [`LocalShell`] for bootstrap tests without
//! SSH. It runs entirely in the Nix build sandbox (no sshd needed).
//!
//! The server accepts public keys from a mutable allow list and from
//! `$HOME/.ssh/authorized_keys` in its temp home, so tests can install keys
//! the way a user would (and key rotation edits take effect). Its host keys
//! can be extended or replaced to simulate multi-key or reinstalled hosts.
//! It also serves direct-tcpip channels by connecting locally.
//!
//! Every process an exec request starts is killed when its channel closes,
//! its session ends, or the test's runtime shuts down.

#![allow(clippy::unwrap_used, clippy::missing_panics_doc)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::keys::{PrivateKey, PublicKey};
use russh::server::{self, Auth, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

use crate::SshPrivateKey;
use crate::bootstrap::RemoteShell;
use crate::bootstrap::authorized_keys::key_identity;
use crate::ssh::{CommandOutput, ExitStatus, HostKey, SshError, SshTarget};

/// The only user the server accepts.
pub const USER: &str = "alice";

struct Shared {
    authorized: Mutex<Vec<PublicKey>>,
    host_keys: Mutex<Vec<PrivateKey>>,
    env: Mutex<Vec<(String, String)>>,
    home: PathBuf,
    sessions: Mutex<Vec<server::Handle>>,
}

/// An in-process SSH server for user [`USER`].
pub struct TestServer {
    /// The address it listens on.
    pub addr: SocketAddr,
    /// The (first) host key presented at start.
    pub host_key: HostKey,
    /// The remote `$HOME`.
    pub home: tempfile::TempDir,
    shared: Arc<Shared>,
    accept_task: tokio::task::JoinHandle<()>,
}

/// A fresh ed25519 key as russh's private key type.
pub fn new_host_key() -> PrivateKey {
    let key = SshPrivateKey::generate_ed25519("").unwrap();
    PrivateKey::from_openssh(&*key.to_openssh().unwrap()).unwrap()
}

impl TestServer {
    /// Starts a server on 127.0.0.1 that accepts `authorized` keys.
    pub async fn start(authorized: Vec<PublicKey>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        Self::serve(listener, authorized)
    }

    /// Like [`Self::start`], on `[::1]`. Fails if the host has no IPv6 loopback.
    pub async fn start_ipv6(authorized: Vec<PublicKey>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("[::1]:0").await?;
        Ok(Self::serve(listener, authorized))
    }

    fn serve(listener: TcpListener, authorized: Vec<PublicKey>) -> Self {
        let addr = listener.local_addr().unwrap();
        let host_private = new_host_key();
        let host_key = HostKey::from_public_key(host_private.public_key());
        let home = tempfile::tempdir().unwrap();
        let shared = Arc::new(Shared {
            authorized: Mutex::new(authorized),
            host_keys: Mutex::new(vec![host_private]),
            env: Mutex::new(Vec::new()),
            home: home.path().to_owned(),
            sessions: Mutex::new(Vec::new()),
        });
        let accept_task = tokio::spawn(accept_loop(listener, shared.clone()));
        Self {
            addr,
            host_key,
            home,
            shared,
            accept_task,
        }
    }

    /// The target for [`USER`] on this server.
    pub fn target(&self) -> SshTarget {
        SshTarget::new(self.addr.ip().to_string(), self.addr.port(), USER)
    }

    /// The remote `$HOME`.
    pub fn home_path(&self) -> &Path {
        self.home.path()
    }

    /// Adds an environment variable for every exec'd command.
    pub fn set_env(&self, key: impl Into<String>, value: impl Into<String>) {
        let mut env = self.shared.env.lock().unwrap();
        env.push((key.into(), value.into()));
    }

    /// Replaces the static allow list (keys in `authorized_keys` still count).
    pub fn set_authorized(&self, keys: Vec<PublicKey>) {
        *self.shared.authorized.lock().unwrap() = keys;
    }

    /// Also offers `key` as a host key on future connections; returns it.
    pub fn add_host_key(&self, key: PrivateKey) -> HostKey {
        let host_key = HostKey::from_public_key(key.public_key());
        self.shared.host_keys.lock().unwrap().push(key);
        host_key
    }

    /// Replaces every host key with a new one on future connections, like a
    /// reinstalled host; returns it.
    pub fn rotate_host_key(&self) -> HostKey {
        let key = new_host_key();
        let host_key = HostKey::from_public_key(key.public_key());
        *self.shared.host_keys.lock().unwrap() = vec![key];
        host_key
    }

    /// Disconnects every client session.
    pub async fn disconnect_all(&self) {
        let handles: Vec<_> = self.shared.sessions.lock().unwrap().drain(..).collect();
        for h in handles {
            let _ = h
                .disconnect(
                    russh::Disconnect::ByApplication,
                    "test shutdown".into(),
                    "en".into(),
                )
                .await;
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    while let Ok((stream, _)) = listener.accept().await {
        let config = Arc::new(server::Config {
            keys: shared.host_keys.lock().unwrap().clone(),
            methods: MethodSet::from(&[MethodKind::PublicKey][..]),
            auth_rejection_time: Duration::from_millis(1),
            auth_rejection_time_initial: Some(Duration::ZERO),
            inactivity_timeout: None,
            ..Default::default()
        });
        let handler = Handler {
            shared: shared.clone(),
            channels: HashMap::new(),
        };
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Ok(running) = server::run_stream(config, stream, handler).await {
                shared.sessions.lock().unwrap().push(running.handle());
                let _ = running.await;
            }
        });
    }
}

/// An exec'd process: its stdin, and a kill switch (dropping it kills the process).
struct Process {
    stdin: mpsc::UnboundedSender<Vec<u8>>,
    _kill: oneshot::Sender<()>,
}

struct Handler {
    shared: Arc<Shared>,
    channels: HashMap<ChannelId, Process>,
}

impl Handler {
    fn is_authorized(&self, user: &str, key: &PublicKey) -> bool {
        if user != USER {
            return false;
        }
        let listed = self.shared.authorized.lock().unwrap().clone();
        if listed.iter().any(|k| k.key_data() == key.key_data()) {
            return true;
        }
        let file = self.shared.home.join(".ssh/authorized_keys");
        let contents = std::fs::read_to_string(file).unwrap_or_default();
        contents.lines().any(|line| {
            key_identity(line)
                .and_then(|(ty, blob)| PublicKey::from_openssh(&format!("{ty} {blob}")).ok())
                .is_some_and(|k| k.key_data() == key.key_data())
        })
    }

    fn command(&self, command: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&self.shared.home)
            .env("HOME", &self.shared.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in self.shared.env.lock().unwrap().iter() {
            cmd.env(k, v);
        }
        cmd
    }
}

/// Forwards `from` to the channel as data (`ext: None`) or extended data.
async fn pump(
    mut from: impl AsyncRead + Unpin,
    handle: server::Handle,
    channel: ChannelId,
    ext: Option<u32>,
) {
    let mut buf = vec![0u8; 32768];
    while let Ok(n) = from.read(&mut buf).await {
        let data = buf[..n].to_vec();
        let sent = match ext {
            _ if n == 0 => break,
            None => handle.data(channel, data).await,
            Some(ext) => handle.extended_data(channel, ext, data).await,
        };
        if sent.is_err() {
            break;
        }
    }
}

impl server::Handler for Handler {
    type Error = russh::Error;

    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(if self.is_authorized(user, key) {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        self.auth_publickey_offered(user, key).await
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Ok(port) = u16::try_from(port_to_connect) else {
            return Ok(());
        };
        let Ok(mut tcp) = tokio::net::TcpStream::connect((host_to_connect, port)).await else {
            return Ok(()); // Dropping `reply` rejects the channel.
        };
        reply.accept().await;
        tokio::spawn(async move {
            let mut stream = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
        });
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).into_owned();
        let Ok(mut child) = self.command(&command).spawn() else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        session.channel_success(channel)?;

        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let mut child_stdin = child.stdin.take().unwrap();
        tokio::spawn(async move {
            while let Some(chunk) = stdin_rx.recv().await {
                if child_stdin.write_all(&chunk).await.is_err() {
                    break;
                }
            }
            // The sender is dropped on EOF: closing stdin tells the child.
        });

        let (kill_tx, kill_rx) = oneshot::channel();
        self.channels.insert(
            channel,
            Process {
                stdin: stdin_tx,
                _kill: kill_tx,
            },
        );
        let handle = session.handle();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        tokio::spawn(async move {
            let run = async {
                tokio::join!(
                    pump(stdout, handle.clone(), channel, None),
                    pump(stderr, handle.clone(), channel, Some(1)),
                );
                child.wait().await
            };
            tokio::select! {
                status = run => {
                    let code = status.ok().and_then(|s| s.code()).unwrap_or(255);
                    let _ = handle.exit_status_request(channel, code as u32).await;
                    let _ = handle.eof(channel).await;
                    let _ = handle.close(channel).await;
                }
                // The channel closed (or the session ended) first: `child`
                // is dropped with this task, which kills it.
                _ = kill_rx => {}
            }
        });
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(process) = self.channels.get(&channel) {
            let _ = process.stdin.send(data.to_vec());
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Close the process's stdin but keep it running.
        if let Some(process) = self.channels.get_mut(&channel) {
            process.stdin = mpsc::unbounded_channel().0;
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.remove(&channel);
        Ok(())
    }
}

/// Runs commands locally with `sh -c` (`HOME` and `PATH` overridable), for
/// bootstrap tests that don't need the SSH layer.
pub struct LocalShell {
    /// `$HOME` for every command.
    pub home: PathBuf,
    /// `$PATH` for every command, if set.
    pub path: Option<String>,
}

impl RemoteShell for LocalShell {
    async fn run(&self, command: &str, stdin: &[u8]) -> Result<CommandOutput, SshError> {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(p) = &self.path {
            cmd.env("PATH", p);
        }
        let mut child = cmd.spawn().map_err(|e| SshError::Other(e.to_string()))?;
        let mut child_stdin = child.stdin.take().unwrap();
        let input = stdin.to_vec();
        let writer = tokio::spawn(async move {
            let _ = child_stdin.write_all(&input).await;
        });
        let out = child
            .wait_with_output()
            .await
            .map_err(|e| SshError::Other(e.to_string()))?;
        let _ = writer.await;
        Ok(CommandOutput {
            status: out
                .status
                .code()
                .map_or(ExitStatus::Unknown, |c| ExitStatus::Code(c as u32)),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
}
