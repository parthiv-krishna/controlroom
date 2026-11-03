//! The fake agent's stand-in for `@controlroom/pi`: a tool call with an `agent` call talks to
//! the session's controlroom agent socket like the extension would (`hello` with the token
//! from the environment, then one call), so daemon tests can drive the agent API end to end.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Calls `method` with `params` on the agent socket; returns the result as JSON text.
pub async fn call(method: &str, params: &Value) -> Result<String> {
    let socket = std::env::var("CONTROLROOM_AGENT_SOCKET")
        .context("CONTROLROOM_AGENT_SOCKET is not set (not launched by controlroom)")?;
    let token = std::env::var("CONTROLROOM_AGENT_TOKEN").context("CONTROLROOM_AGENT_TOKEN")?;
    let stream = UnixStream::connect(&socket)
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
        if let Some(error) = message.get("error") {
            bail!("{}", error["message"].as_str().unwrap_or("agent API error"));
        }
        if message["id"] == 2 {
            return Ok(message["result"].to_string());
        }
    }
    bail!("the daemon closed the agent socket")
}
