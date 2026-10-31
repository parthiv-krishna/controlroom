//! Exec channels: a remote command's stdio as an async duplex stream.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};

use russh::client::Msg;
use russh::{Channel, ChannelMsg};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::{mpsc, oneshot, watch};

/// Bytes of stderr kept per exec channel; the rest is dropped.
pub const STDERR_LIMIT: usize = 64 * 1024;

/// How a remote command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitStatus {
    /// The command exited with this code.
    Code(u32),
    /// The command was killed by a signal.
    Signal {
        /// The signal's name, e.g. `TERM`.
        signal: String,
        /// Whether it dumped core.
        core_dumped: bool,
        /// The server's error message, if any.
        message: String,
    },
    /// The channel closed without an exit status (e.g. the exec request was
    /// rejected or the connection dropped).
    Unknown,
}

impl ExitStatus {
    /// Whether the command exited with code 0.
    pub fn success(&self) -> bool {
        matches!(self, ExitStatus::Code(0))
    }
}

impl std::fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExitStatus::Code(c) => write!(f, "exit code {c}"),
            ExitStatus::Signal { signal, .. } => write!(f, "killed by signal {signal}"),
            ExitStatus::Unknown => f.write_str("no exit status"),
        }
    }
}

/// Output of [`super::SshConnection::run`].
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// How the command ended.
    pub status: ExitStatus,
    /// Everything it wrote to stdout.
    pub stdout: Vec<u8>,
    /// Its stderr, up to [`STDERR_LIMIT`] bytes.
    pub stderr: Vec<u8>,
}

impl CommandOutput {
    /// Stdout as text (lossy UTF-8).
    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// Stderr as text (lossy UTF-8).
    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

#[derive(Debug, Clone, Default)]
struct Progress {
    exit: Option<ExitStatus>,
    closed: bool,
}

/// Closes the SSH channel once every part of an [`ExecChannel`] is dropped.
struct CloseGuard(Option<oneshot::Sender<()>>);

impl Drop for CloseGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

/// A running remote command: a general duplex byte stream multiplexed on its
/// connection (the daemon's JSON-RPC bridge, uploads, terminal attach, ...).
///
/// Implements [`AsyncRead`] (the command's stdout) and [`AsyncWrite`] (its
/// stdin; `shutdown` sends EOF). Stderr is captured, up to [`STDERR_LIMIT`]
/// bytes, and available from [`Self::stderr`]. Dropping every part closes the
/// channel.
///
/// Stdout is delivered with backpressure: read it (or drop the reader via
/// [`Self::into_parts`]) before awaiting the exit status, or the command may
/// block on a full pipe. russh grants the server more window as data arrives,
/// not as it is consumed, so a reader that stops reading eventually stalls
/// the whole connection (every channel and keepalives), not just this
/// channel. Consumers must keep reading, or drop the reader.
pub struct ExecChannel {
    stdout: ExecReader,
    stdin: ExecWriter,
    exit: ExitWatcher,
}

/// The stdout half of an [`ExecChannel`].
pub struct ExecReader {
    rx: mpsc::Receiver<bytes::Bytes>,
    pending: bytes::Bytes,
    _guard: Arc<CloseGuard>,
}

/// The stdin half of an [`ExecChannel`].
pub struct ExecWriter {
    inner: Pin<Box<dyn AsyncWrite + Send>>,
    _guard: Arc<CloseGuard>,
}

/// Waits for an [`ExecChannel`]'s exit status; also exposes captured stderr.
#[derive(Clone)]
pub struct ExitWatcher {
    progress: watch::Receiver<Progress>,
    stderr: Arc<Mutex<Vec<u8>>>,
    _guard: Arc<CloseGuard>,
}

impl ExecChannel {
    pub(crate) async fn start(channel: Channel<Msg>, command: &str) -> Result<Self, russh::Error> {
        channel.exec(true, command).await?;
        let (mut read_half, write_half) = channel.split();
        let writer = write_half.make_writer();

        let (data_tx, data_rx) = mpsc::channel::<bytes::Bytes>(16);
        let (progress_tx, progress_rx) = watch::channel(Progress::default());
        let (close_tx, mut close_rx) = oneshot::channel::<()>();
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let stderr_pump = stderr.clone();

        tokio::spawn(async move {
            let mut data_tx = Some(data_tx);
            loop {
                tokio::select! {
                    msg = read_half.wait() => match msg {
                        Some(ChannelMsg::Data { data }) => {
                            if let Some(tx) = &data_tx
                                && tx.send(data).await.is_err()
                            {
                                // Reader dropped: discard further stdout.
                                data_tx = None;
                            }
                        }
                        Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                            let mut buf = stderr_pump.lock().unwrap_or_else(|p| p.into_inner());
                            let room = STDERR_LIMIT.saturating_sub(buf.len());
                            buf.extend_from_slice(&data[..data.len().min(room)]);
                        }
                        Some(ChannelMsg::ExitStatus { exit_status }) => {
                            progress_tx.send_modify(|p| p.exit = Some(ExitStatus::Code(exit_status)));
                        }
                        Some(ChannelMsg::ExitSignal { signal_name, core_dumped, error_message, .. }) => {
                            progress_tx.send_modify(|p| {
                                p.exit = Some(ExitStatus::Signal {
                                    signal: format!("{signal_name:?}"),
                                    core_dumped,
                                    message: error_message,
                                })
                            });
                        }
                        Some(ChannelMsg::Failure) => {
                            // The exec request itself was refused.
                            tracing::debug!("exec request rejected by server");
                            let _ = write_half.close().await;
                            break;
                        }
                        Some(ChannelMsg::Eof) => data_tx = None,
                        Some(ChannelMsg::Close) | None => break,
                        Some(_) => {}
                    },
                    _ = &mut close_rx => {
                        let _ = write_half.close().await;
                        break;
                    }
                }
            }
            drop(data_tx);
            progress_tx.send_modify(|p| p.closed = true);
        });

        let guard = Arc::new(CloseGuard(Some(close_tx)));
        Ok(Self {
            stdout: ExecReader {
                rx: data_rx,
                pending: bytes::Bytes::new(),
                _guard: guard.clone(),
            },
            stdin: ExecWriter {
                inner: Box::pin(writer),
                _guard: guard.clone(),
            },
            exit: ExitWatcher {
                progress: progress_rx,
                stderr,
                _guard: guard,
            },
        })
    }

    /// Splits into stdout, stdin and the exit watcher, to use them from different tasks.
    pub fn into_parts(self) -> (ExecReader, ExecWriter, ExitWatcher) {
        (self.stdout, self.stdin, self.exit)
    }

    /// Captured stderr so far (at most [`STDERR_LIMIT`] bytes).
    pub fn stderr(&self) -> Vec<u8> {
        self.exit.stderr()
    }

    /// Waits for the channel to close and returns the exit status.
    pub async fn exit_status(&mut self) -> ExitStatus {
        self.exit.wait().await
    }

    /// Writes `stdin` (then EOF) while collecting stdout, and waits for exit.
    pub async fn collect(self, stdin: &[u8]) -> std::io::Result<CommandOutput> {
        use tokio::io::AsyncReadExt;
        let (mut reader, mut writer, mut exit) = self.into_parts();
        let write = async {
            if !stdin.is_empty() {
                writer.write_all(stdin).await?;
            }
            writer.shutdown().await?;
            drop(writer);
            Ok::<_, std::io::Error>(())
        };
        let mut stdout = Vec::new();
        let read = reader.read_to_end(&mut stdout);
        let (w, r) = tokio::join!(write, read);
        r?;
        // A command may exit without reading its stdin; that is not an error.
        if let Err(e) = w {
            tracing::debug!("exec stdin write ended early: {e}");
        }
        drop(reader);
        let status = exit.wait().await;
        Ok(CommandOutput {
            status,
            stdout,
            stderr: exit.stderr(),
        })
    }
}

impl ExitWatcher {
    /// Waits for the channel to close and returns the exit status.
    pub async fn wait(&mut self) -> ExitStatus {
        let progress = self
            .progress
            .wait_for(|p| p.closed)
            .await
            .map(|p| p.clone())
            .unwrap_or_default();
        progress.exit.unwrap_or(ExitStatus::Unknown)
    }

    /// The exit status, if one has arrived already.
    pub fn try_status(&self) -> Option<ExitStatus> {
        self.progress.borrow().exit.clone()
    }

    /// Captured stderr so far (at most [`STDERR_LIMIT`] bytes).
    pub fn stderr(&self) -> Vec<u8> {
        self.stderr
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl AsyncRead for ExecReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        while self.pending.is_empty() {
            match ready!(self.rx.poll_recv(cx)) {
                Some(data) => self.pending = data,
                None => return Poll::Ready(Ok(())),
            }
        }
        let n = self.pending.len().min(buf.remaining());
        buf.put_slice(&self.pending.split_to(n));
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for ExecWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.inner.as_mut().poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.inner.as_mut().poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.inner.as_mut().poll_shutdown(cx)
    }
}

impl AsyncRead for ExecChannel {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for ExecChannel {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}
