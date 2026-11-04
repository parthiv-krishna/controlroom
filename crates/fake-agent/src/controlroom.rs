//! The fake agent's stand-in for `@controlroom/pi`: a tool call with `agent` calls talks to the
//! session's controlroom agent socket like the extension would (`hello` with the token from the
//! environment, then the call), so daemon tests can drive the agent API end to end.
//!
//! Like the extension, a call whose connection closes before its answer (the agent daemon
//! restarted) is sent again, with the same params and so the same `call_id`, on a new
//! connection, until [`RESEND_FOR`] has passed.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::Instant;

/// How long a call keeps reconnecting after its connection was lost.
const RESEND_FOR: Duration = Duration::from_secs(120);

/// The pause between reconnection attempts.
const RECONNECT_EVERY: Duration = Duration::from_millis(50);

/// How a call ended.
pub enum Answer {
    /// The JSON result.
    Result(Value),
    /// The JSON-RPC error object.
    Error(Value),
}

/// Calls `method` with `params` on the agent socket, resending it after a lost connection.
pub async fn call(method: &str, params: &Value) -> Result<Answer> {
    let socket = std::env::var("CONTROLROOM_AGENT_SOCKET")
        .context("CONTROLROOM_AGENT_SOCKET is not set (not launched by controlroom)")?;
    let token = std::env::var("CONTROLROOM_AGENT_TOKEN").context("CONTROLROOM_AGENT_TOKEN")?;
    let mut lost_since: Option<Instant> = None;
    loop {
        match call_once(&socket, &token, method, params).await {
            Ok(answer) => return Ok(answer),
            Err(e) => {
                let since = *lost_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= RESEND_FOR {
                    return Err(
                        e.context(format!("the agent socket stayed away for {RESEND_FOR:?}"))
                    );
                }
                tokio::time::sleep(RECONNECT_EVERY).await;
            }
        }
    }
}

/// One attempt on a fresh connection; an `Err` means the connection failed or closed.
async fn call_once(socket: &str, token: &str, method: &str, params: &Value) -> Result<Answer> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to {socket}"))?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let hello = json!({"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {
        "protocol_version": {"major": 1, "minor": 0}, "token": token,
        "client_name": "controlroom-fake-agent"
    }});
    let request = json!({"jsonrpc": "2.0", "id": 2, "method": method, "params": params});
    write
        .write_all(format!("{hello}\n{request}\n").as_bytes())
        .await?;
    while let Some(line) = lines.next_line().await? {
        let message: Value = serde_json::from_str(&line)?;
        if message["id"] == 1
            && let Some(error) = message.get("error")
        {
            // A refused hello is final.
            return Ok(Answer::Error(error.clone()));
        }
        if message["id"] == 2 {
            return Ok(match message.get("error") {
                Some(error) => Answer::Error(error.clone()),
                None => Answer::Result(message["result"].clone()),
            });
        }
    }
    bail!("the daemon closed the agent socket")
}
