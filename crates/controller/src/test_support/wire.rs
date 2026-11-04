//! Serving one newline-delimited JSON-RPC connection for the fakes: requests go to a handler,
//! and the test can push notifications through a [`Peer`] while the connection is up.

use std::future::Future;
use std::sync::{Arc, Mutex};

use controlroom_protocol::jsonrpc::encode_line;
use controlroom_protocol::{Message, NotificationMethod, RpcError, RpcNotification, RpcResponse};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

/// The sending side of a live connection.
#[derive(Clone)]
pub struct Peer {
    out: mpsc::UnboundedSender<String>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl Peer {
    /// Sends a notification.
    pub fn notify<N: NotificationMethod>(&self, params: N::Params) {
        let line = encode_line(&RpcNotification::new::<N>(params)).unwrap();
        let _ = self.out.send(line);
    }

    /// Drops the connection.
    pub fn close(&self) {
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}

/// Serves `stream` until it closes: every request is answered by `handle(peer, method,
/// params)`, each in its own task (so a held call doesn't block the others).
pub fn serve<S, H, F>(stream: S, handle: H) -> Peer
where
    S: AsyncRead + AsyncWrite + Send + 'static,
    H: Fn(Peer, String, Value) -> F + Send + Sync + 'static,
    F: Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let task: Arc<Mutex<Option<JoinHandle<()>>>> = Arc::default();
    let peer = Peer {
        out: out_tx,
        task: task.clone(),
    };
    let handle = Arc::new(handle);
    let me = peer.clone();
    // One task owns both halves, so aborting it closes the connection; dropping its `calls`
    // aborts the calls still running.
    let run = tokio::spawn(async move {
        let (reader, mut writer) = tokio::io::split(stream);
        let mut lines = BufReader::new(reader).lines();
        let mut calls = JoinSet::new();
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Ok(Some(line)) = line else { break };
                    let Ok(Message::Request(request)) = Message::parse(&line) else {
                        continue;
                    };
                    let (handle, peer) = (handle.clone(), me.clone());
                    calls.spawn(async move {
                        let response =
                            match handle(peer.clone(), request.method, request.params).await {
                                Ok(result) => RpcResponse::success(request.id, result),
                                Err(error) => RpcResponse::error(Some(request.id), error),
                            };
                        let _ = peer.out.send(encode_line(&response).unwrap());
                    });
                }
                Some(line) = out_rx.recv() => {
                    if writer.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                }
                Some(_) = calls.join_next(), if !calls.is_empty() => {}
            }
        }
    });
    *task.lock().unwrap() = Some(run);
    peer
}
