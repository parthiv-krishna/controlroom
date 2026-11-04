//! A local, deterministic OpenAI-compatible chat-completions server.
//!
//! pi talks to it through an isolated `models.json` (`api:
//! "openai-completions"`). Each request consumes the next scripted
//! [`Reply`]; when the script is exhausted the server answers `ok`. Streams
//! can stop at named gates that the recorder releases, which makes "abort
//! during text" or "steer mid-run" deterministic without sleeps.
//!
//! [`MockModel::serve`] adds an HTTP control API ([`control`]) for tests that
//! script the model from another process, and can require an API key, so a
//! test sees that the credential reached the model (answers 401 otherwise).

pub mod control;
mod script;
mod sse;

use std::collections::{BTreeSet, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use serde_json::Value;
use tokio::sync::{mpsc, watch};

pub use script::{Finish, Part, Reply, StreamReply, chunks};

/// Shared state of a running mock.
struct Shared {
    replies: Mutex<VecDeque<Reply>>,
    /// Bodies of all requests received, in order.
    requests: Mutex<Vec<Value>>,
    /// Gates released by the recorder.
    released: watch::Sender<BTreeSet<String>>,
    /// Gates a stream is currently waiting at (or has passed).
    reached: watch::Sender<BTreeSet<String>>,
    /// Completions without `Authorization: Bearer <key>` are refused.
    api_key: Option<String>,
}

/// A running mock model server. Dropping it stops the server.
pub struct MockModel {
    addr: SocketAddr,
    shared: Arc<Shared>,
    server: tokio::task::JoinHandle<()>,
}

impl MockModel {
    /// Serve `replies` on an ephemeral loopback port.
    pub async fn start(replies: Vec<Reply>) -> Result<Self> {
        Self::bind(SocketAddr::from(([127, 0, 0, 1], 0)), replies, false, None).await
    }

    /// Serve on `addr` with no scripted replies yet, plus the [`control`] API that scripts
    /// the model over HTTP. With `api_key`, completions must carry `Authorization: Bearer
    /// <api_key>` (else 401, and no reply is used).
    pub async fn serve(addr: SocketAddr, api_key: Option<String>) -> Result<Self> {
        Self::bind(addr, Vec::new(), true, api_key).await
    }

    async fn bind(
        addr: SocketAddr,
        replies: Vec<Reply>,
        with_control: bool,
        api_key: Option<String>,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
            released: watch::Sender::new(BTreeSet::new()),
            reached: watch::Sender::new(BTreeSet::new()),
            api_key,
        });
        let mut app = Router::new().route("/v1/chat/completions", post(completions));
        if with_control {
            app = app.merge(control::routes());
        }
        let app = app.with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding the mock model to {addr}"))?;
        let addr = listener.local_addr()?;
        let server = tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app).await {
                eprintln!("mock model server failed: {err}");
            }
        });
        Ok(Self {
            addr,
            shared,
            server,
        })
    }

    /// `http://127.0.0.1:<port>/v1`, the `baseUrl` for `models.json`.
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    /// The port the server listens on.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Appends scripted replies (for replies that depend on what the agent did so far).
    pub fn push(&self, replies: impl IntoIterator<Item = Reply>) {
        lock(&self.shared.replies).extend(replies);
    }

    /// Let streams waiting at `gate` continue.
    pub fn release(&self, gate: &str) {
        self.shared.released.send_modify(|set| {
            set.insert(gate.to_owned());
        });
    }

    /// Wait until a stream reaches `gate`.
    pub async fn wait_reached(&self, gate: &str, timeout: Duration) -> Result<()> {
        let mut rx = self.shared.reached.subscribe();
        tokio::time::timeout(timeout, rx.wait_for(|set| set.contains(gate)))
            .await
            .map_err(|_| anyhow!("timed out waiting for the model to reach gate {gate}"))?
            .map_err(|_| anyhow!("mock model stopped"))?;
        Ok(())
    }

    /// Request bodies received so far.
    pub fn requests(&self) -> Vec<Value> {
        lock(&self.shared.requests).clone()
    }

    /// Scripted replies not consumed yet.
    pub fn remaining(&self) -> usize {
        lock(&self.shared.replies).len()
    }

    /// Runs until the server stops (it only stops on an error).
    pub async fn wait(mut self) -> Result<()> {
        (&mut self.server)
            .await
            .context("the mock model server task")
    }
}

impl Drop for MockModel {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Every critical section is a single push/pop, so a poisoned lock still
    // holds consistent data.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn completions(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Some(key) = &shared.api_key {
        let sent = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        if sent != Some(format!("Bearer {key}").as_str()) {
            return error_response(401, "invalid API key");
        }
    }
    let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("mock-model")
        .to_owned();
    let index = {
        let mut requests = lock(&shared.requests);
        requests.push(request);
        requests.len()
    };
    let reply = lock(&shared.replies)
        .pop_front()
        .unwrap_or_else(|| Reply::text("ok"));
    match reply {
        Reply::Error { status, message } => error_response(status, &message),
        Reply::Stream(stream) => {
            let (tx, rx) = mpsc::channel::<String>(16);
            let completion = sse::Completion::new(format!("chatcmpl-mock-{index}"), model);
            tokio::spawn(produce(shared, completion, stream, tx));
            let body = futures::stream::unfold(rx, |mut rx| async move {
                rx.recv()
                    .await
                    .map(|frame| (Ok::<_, std::convert::Infallible>(frame), rx))
            });
            Response::builder()
                .header(header::CONTENT_TYPE, "text/event-stream")
                .header(header::CACHE_CONTROL, "no-cache")
                .body(Body::from_stream(body))
                .unwrap_or_else(|_| error_response(500, "building the response failed"))
        }
    }
}

fn error_response(status: u16, message: &str) -> Response {
    let body = serde_json::json!({
        "error": {"message": message, "type": "mock_error", "code": status}
    });
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_default()
}

/// Write the SSE frames of one stream; stops early when the client goes away.
async fn produce(
    shared: Arc<Shared>,
    completion: sse::Completion,
    stream: StreamReply,
    tx: mpsc::Sender<String>,
) {
    let mut tool_index = 0;
    for part in &stream.parts {
        let frames = match part {
            Part::Reasoning(chunks) => chunks.iter().map(|c| completion.reasoning(c)).collect(),
            Part::Text(chunks) => chunks.iter().map(|c| completion.text(c)).collect(),
            Part::ToolCall { id, name, args } => {
                let frames = completion.tool_call(tool_index, id, name, args);
                tool_index += 1;
                frames
            }
            Part::Gate(gate) => {
                shared.reached.send_modify(|set| {
                    set.insert(gate.clone());
                });
                let mut released = shared.released.subscribe();
                tokio::select! {
                    _ = released.wait_for(|set| set.contains(gate)) => {}
                    () = tx.closed() => return,
                }
                Vec::new()
            }
        };
        for frame in frames {
            if tx.send(frame).await.is_err() {
                return;
            }
        }
    }
    for frame in [
        completion.finish(stream.finish, stream.completion_tokens),
        sse::DONE.to_owned(),
    ] {
        if tx.send(frame).await.is_err() {
            return;
        }
    }
}
