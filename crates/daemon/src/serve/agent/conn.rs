//! One agent connection: newline-delimited JSON-RPC, `hello` with the session's token first,
//! then the agent API calls (each on its own task, a few at a time) and `state` notifications
//! whenever the session's todo list, open questions or auto-continue change.

use std::sync::Arc;

use controlroom_protocol::agent::notifications::State;
use controlroom_protocol::agent::{AgentHelloParams, AgentHelloResult, AgentRequest, methods};
use controlroom_protocol::ids::SessionId;
use controlroom_protocol::jsonrpc::{encode_line, parse_params};
use controlroom_protocol::{
    AGENT_PROTOCOL_VERSION, Ack, ErrorData, Message, Method, RpcError, RpcId, RpcNotification,
    RpcRequest, RpcResponse,
};
use controlroom_runtime::Runtime;
use controlroom_sandbox::webhook::HookSecret;
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Semaphore, mpsc};

use crate::dirs::Dirs;
use crate::launch::load_agent_token;
use crate::version::VERSION;

/// Calls of one connection running at once.
const MAX_IN_FLIGHT: usize = 8;
/// Outgoing lines buffered per connection.
const OUT_QUEUE: usize = 64;
/// Longest request line.
const MAX_LINE_BYTES: u64 = 1 << 20;

/// Serves one connection until the extension goes away.
pub async fn serve(stream: UnixStream, runtime: Runtime, dirs: Dirs, id: SessionId) {
    let (read, mut write) = stream.into_split();
    let (out, mut lines) = mpsc::channel::<String>(OUT_QUEUE);
    let writer = tokio::spawn(async move {
        while let Some(line) = lines.recv().await {
            if write.write_all(line.as_bytes()).await.is_err() {
                return;
            }
        }
        let _ = write.shutdown().await;
    });
    let mut reader = BufReader::new(read);
    let slots = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let mut state_task = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        let mut limited = (&mut reader).take(MAX_LINE_BYTES + 1);
        match limited.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) if line.len() as u64 > MAX_LINE_BYTES => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line);
        if text.trim().is_empty() {
            continue;
        }
        let request = match Message::parse(&text) {
            Ok(Message::Request(request)) => request,
            Ok(Message::Notification(_) | Message::Response(_)) => continue,
            Err(e) => {
                send(&out, &RpcResponse::<Value>::error(e.id, *e.error)).await;
                continue;
            }
        };
        if state_task.is_none() {
            let request_id = request.id.clone();
            let result = hello(&dirs, id, request);
            let ok = result.is_ok();
            respond(&out, request_id, result).await;
            if ok {
                state_task = Some(tokio::spawn(push_state(runtime.clone(), id, out.clone())));
            }
            continue;
        }
        let Ok(slot) = slots.clone().acquire_owned().await else {
            break;
        };
        let (runtime, out) = (runtime.clone(), out.clone());
        tokio::spawn(async move {
            let request_id = request.id.clone();
            let result = handle(&runtime, id, request).await;
            respond(&out, request_id, result).await;
            drop(slot);
        });
    }
    if let Some(task) = state_task {
        task.abort();
    }
    drop(out);
    let _ = writer.await;
}

/// The first call: `hello` with the session's current token.
fn hello(dirs: &Dirs, id: SessionId, request: RpcRequest<Value>) -> Result<Value, RpcError> {
    if request.method != methods::Hello::NAME {
        return Err(RpcError::new(
            ErrorData::HelloRequired,
            format!("call hello before {}", request.method),
        ));
    }
    let params: AgentHelloParams = parse_params(methods::Hello::NAME, request.params)?;
    if !params
        .protocol_version
        .is_compatible_with(AGENT_PROTOCOL_VERSION)
    {
        return Err(RpcError::new(
            ErrorData::IncompatibleVersion {
                protocol_version: AGENT_PROTOCOL_VERSION,
                version: VERSION.to_owned(),
            },
            format!(
                "agent protocol {} is incompatible with this daemon's {AGENT_PROTOCOL_VERSION}",
                params.protocol_version
            ),
        ));
    }
    let expected = load_agent_token(dirs, &id.to_string());
    let given = HookSecret::parse(params.token.trim());
    let valid = matches!((&expected, &given), (Some(e), Some(g)) if e.matches(g));
    if !valid {
        tracing::warn!("session {id}: agent hello with a wrong token");
        return Err(RpcError::new(
            ErrorData::Unauthenticated,
            "wrong or outdated agent token",
        ));
    }
    tracing::info!("session {id}: agent connected ({})", params.client_name);
    json(Ok(AgentHelloResult {
        daemon_version: VERSION.to_owned(),
        protocol_version: AGENT_PROTOCOL_VERSION,
        session_id: id,
        capabilities: Vec::new(),
    }))
}

/// Routes one call after `hello`.
async fn handle(
    runtime: &Runtime,
    id: SessionId,
    request: RpcRequest<Value>,
) -> Result<Value, RpcError> {
    match AgentRequest::from_request(request)? {
        AgentRequest::Hello(_) => Err(RpcError::new(
            ErrorData::InvalidRequest,
            "hello was already called on this connection",
        )),
        AgentRequest::QuestionAsk(p) => json(runtime.agent_ask(id, p).await),
        AgentRequest::QuestionWithdraw(p) => json(runtime.agent_withdraw(id, p).await),
        AgentRequest::TodoList(_) => json(runtime.agent_todos(id).await),
        AgentRequest::TodoAdd(p) => json(runtime.agent_todo_add(id, p).await),
        AgentRequest::TodoUpdate(p) => json(runtime.agent_todo_update(id, p).await),
        AgentRequest::Notify(p) => json(runtime.agent_notify(id, p).await.map(|()| Ack {})),
        AgentRequest::JobRun(p) => json(runtime.agent_job_run(id, p).await),
        AgentRequest::JobFollow(p) => json(runtime.agent_job_follow(id, p).await),
        AgentRequest::JobUnfollow(p) => json(runtime.agent_job_unfollow(id, p).await),
        AgentRequest::JobStatus(p) => json(runtime.agent_job_status(id, p).await),
        AgentRequest::JobKill(p) => json(runtime.agent_job_kill(id, p).await),
    }
}

/// Sends the session's agent state now and on every change.
async fn push_state(runtime: Runtime, id: SessionId, out: mpsc::Sender<String>) {
    let mut states = match runtime.agent_state(id).await {
        Ok(states) => states,
        Err(e) => {
            tracing::warn!("session {id}: agent state: {e}");
            return;
        }
    };
    loop {
        let state = states.borrow_and_update().clone();
        if let Some(state) = state {
            send(&out, &RpcNotification::new::<State>(state)).await;
        }
        if states.changed().await.is_err() {
            return;
        }
    }
}

fn json<T: Serialize>(result: Result<T, RpcError>) -> Result<Value, RpcError> {
    result.and_then(|value| {
        serde_json::to_value(value)
            .map_err(|e| RpcError::new(ErrorData::InternalError, e.to_string()))
    })
}

async fn respond(out: &mpsc::Sender<String>, id: RpcId, result: Result<Value, RpcError>) {
    let response = match result {
        Ok(value) => RpcResponse::success(id, value),
        Err(error) => RpcResponse::error(Some(id), error),
    };
    send(out, &response).await;
}

async fn send<T: Serialize>(out: &mpsc::Sender<String>, message: &T) {
    match encode_line(message) {
        Ok(line) => {
            let _ = out.send(line).await;
        }
        Err(e) => tracing::error!("encoding an agent message: {e}"),
    }
}
