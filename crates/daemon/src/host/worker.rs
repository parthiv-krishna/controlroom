//! The per-workspace fs worker: `controlroomd workspace-exec`, started by the host daemon under
//! the workspace's nono profile, serves the file and search ops ([`super::fsops`]) so the
//! unsandboxed daemon never touches project files on the agent's behalf.
//!
//! Protocol: newline-delimited JSON on the worker's stdin and stdout. Requests are
//! `{"id": n, "op": HostOp}`; each answer is `{"id": n, "result": HostResult}` or
//! `{"id": n, "error": RpcError}`, in completion order (ops run concurrently, a few at a
//! time). The worker exits when its stdin closes; it keeps nothing on disk.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};

use controlroom_protocol::RpcError;
use controlroom_protocol::host::{HostOp, HostResult};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot};

use super::fsops::{self, Context, LineIndexCache};
use super::store::internal;

/// Ops a worker runs at once.
const CONCURRENCY: usize = 8;

/// One request to the worker.
#[derive(Debug, Serialize, Deserialize)]
struct Request {
    id: u64,
    op: HostOp,
}

/// One answer from the worker.
#[derive(Debug, Serialize, Deserialize)]
struct Response {
    id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<HostResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

/// Arguments of `controlroomd workspace-exec`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The workspace root: relative paths resolve against it.
    #[arg(long)]
    root: String,
    /// The home directory `~` stands for.
    #[arg(long)]
    home: String,
}

/// Runs the worker until stdin closes.
pub fn run(args: Args) -> ExitCode {
    let ctx = Arc::new(Context {
        root: args.root,
        home: args.home,
    });
    let cache = Arc::new(LineIndexCache::default());
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let (slots_tx, slots_rx) = std::sync::mpsc::sync_channel::<()>(CONCURRENCY);
    let slots_rx = Arc::new(Mutex::new(slots_rx));
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(e) => {
                eprintln!("workspace-exec: bad request: {e}");
                continue;
            }
        };
        if slots_tx.send(()).is_err() {
            break;
        }
        let (ctx, cache, out, slots_rx) =
            (ctx.clone(), cache.clone(), out.clone(), slots_rx.clone());
        std::thread::spawn(move || {
            let outcome = fsops::run(&ctx, &cache, request.op);
            let response = match outcome {
                Ok(result) => Response {
                    id: request.id,
                    result: Some(result),
                    error: None,
                },
                Err(error) => Response {
                    id: request.id,
                    result: None,
                    error: Some(error),
                },
            };
            if let Ok(mut text) = serde_json::to_string(&response) {
                text.push('\n');
                let mut out = out.lock().unwrap_or_else(PoisonError::into_inner);
                let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
            }
            let _ = slots_rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
        });
    }
    ExitCode::SUCCESS
}

type Pending = Arc<Mutex<Option<HashMap<u64, oneshot::Sender<Result<HostResult, RpcError>>>>>>;

/// The daemon's handle on a running worker.
pub struct WorkerClient {
    requests: mpsc::Sender<String>,
    pending: Pending,
    next_id: std::sync::atomic::AtomicU64,
}

impl WorkerClient {
    /// Takes over a spawned worker (stdin and stdout piped).
    pub fn new(mut child: Child) -> std::io::Result<Self> {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("worker stdin is not piped"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("worker stdout is not piped"))?;
        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
        let (requests, requests_rx) = mpsc::channel(64);
        tokio::spawn(send(stdin, requests_rx));
        let receiving = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(response) = serde_json::from_str::<Response>(&line) else {
                    tracing::warn!("fs worker: unreadable answer");
                    continue;
                };
                let reply = receiving
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_mut()
                    .and_then(|p| p.remove(&response.id));
                if let Some(reply) = reply {
                    let outcome = match (response.result, response.error) {
                        (_, Some(error)) => Err(error),
                        (Some(result), None) => Ok(result),
                        (None, None) => Err(internal("the fs worker sent an empty answer")),
                    };
                    let _ = reply.send(outcome);
                }
            }
            // The worker is gone: fail what it still owed, and every later call.
            let owed = receiving
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .unwrap_or_default();
            for (_, reply) in owed {
                let _ = reply.send(Err(internal("the fs worker exited")));
            }
            let _ = child.kill().await;
        });
        Ok(Self {
            requests,
            pending,
            next_id: std::sync::atomic::AtomicU64::new(1),
        })
    }

    /// Whether the worker still runs.
    pub fn is_alive(&self) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// Runs one op in the worker.
    pub async fn call(&self, op: HostOp) -> Result<HostResult, RpcError> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let line = serde_json::to_string(&Request { id, op }).map_err(internal)?;
        let (tx, rx) = oneshot::channel();
        match self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            Some(pending) => pending.insert(id, tx),
            None => return Err(internal("the fs worker exited")),
        };
        self.requests
            .send(line)
            .await
            .map_err(|_| internal("the fs worker exited"))?;
        rx.await.map_err(|_| internal("the fs worker exited"))?
    }
}

async fn send(mut stdin: ChildStdin, mut requests: mpsc::Receiver<String>) {
    while let Some(mut line) = requests.recv().await {
        line.push('\n');
        if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
            return;
        }
    }
}
