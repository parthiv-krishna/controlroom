//! A blocking JSON-RPC client for the daemon socket, speaking `hello` as a debug client.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use controlroom_protocol::daemon::{ClientRole, HelloParams, HelloResult, methods};
use controlroom_protocol::jsonrpc::{JsonRpcVersion, encode_line};
use controlroom_protocol::{
    DAEMON_PROTOCOL_VERSION, ErrorData, Message, Method, RpcError, RpcId, RpcNotification,
    RpcRequest,
};
use serde_json::Value;

use crate::version::VERSION;

/// Default timeout of one call.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a call failed.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// The daemon answered with an error.
    #[error("{}: {}", kind(.0), .0.message)]
    Rpc(RpcError),
    /// The connection failed or timed out.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The daemon closed the connection.
    #[error("the daemon closed the connection")]
    Closed,
    /// A message from the daemon didn't parse.
    #[error("bad message from the daemon: {0}")]
    Decode(String),
}

/// The error kind's wire name, e.g. `unsupported`.
fn kind(error: &RpcError) -> String {
    error
        .data
        .as_ref()
        .and_then(|d| serde_json::to_value(d).ok())
        .and_then(|v| v.get("kind").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| format!("code {}", error.code))
}

/// A connection to the daemon.
pub struct DaemonClient {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: i64,
    /// Notifications that arrived while waiting for a response.
    notifications: VecDeque<RpcNotification<Value>>,
}

impl DaemonClient {
    /// Wraps a connected stream; calls time out after [`CALL_TIMEOUT`].
    pub fn new(stream: UnixStream) -> std::io::Result<Self> {
        stream.set_read_timeout(Some(CALL_TIMEOUT))?;
        Ok(Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
            next_id: 0,
            notifications: VecDeque::new(),
        })
    }

    /// `hello` as a debug client.
    pub fn hello(&mut self) -> Result<HelloResult, CallError> {
        self.call::<methods::Hello>(HelloParams {
            protocol_version: DAEMON_PROTOCOL_VERSION,
            client_name: format!("controlroomd ctl {VERSION}"),
            role: ClientRole::Debug,
        })
    }

    /// Calls `M`.
    pub fn call<M: Method>(&mut self, params: M::Params) -> Result<M::Result, CallError> {
        let params = serde_json::to_value(params).map_err(|e| CallError::Decode(e.to_string()))?;
        let result = self.call_raw(M::NAME, params)?;
        serde_json::from_value(result).map_err(|e| {
            CallError::Rpc(RpcError::new(
                ErrorData::InvalidResponse,
                format!("malformed {} result: {e}", M::NAME),
            ))
        })
    }

    /// Calls `method` with untyped params and returns the untyped result.
    pub fn call_raw(&mut self, method: &str, params: Value) -> Result<Value, CallError> {
        self.next_id += 1;
        let id = RpcId::Number(self.next_id);
        let request = RpcRequest {
            jsonrpc: JsonRpcVersion,
            id: id.clone(),
            method: method.to_owned(),
            params,
        };
        let line = encode_line(&request).map_err(|e| CallError::Decode(e.to_string()))?;
        self.writer.write_all(line.as_bytes())?;
        loop {
            match self.next_message()? {
                Message::Response(response) if response.id.as_ref() == Some(&id) => {
                    return response.into_result().map_err(CallError::Rpc);
                }
                Message::Notification(n) => self.notifications.push_back(n),
                _ => {}
            }
        }
    }

    /// The next notification, waiting without a timeout.
    pub fn next_notification(&mut self) -> Result<RpcNotification<Value>, CallError> {
        if let Some(n) = self.notifications.pop_front() {
            return Ok(n);
        }
        self.reader.get_ref().set_read_timeout(None)?;
        loop {
            if let Message::Notification(n) = self.next_message()? {
                return Ok(n);
            }
        }
    }

    fn next_message(&mut self) -> Result<Message, CallError> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(CallError::Closed);
        }
        Message::parse(&line).map_err(|e| CallError::Decode(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_name_their_kind() {
        let e = CallError::Rpc(RpcError::new(ErrorData::Unsupported, "no runtime yet"));
        assert_eq!(e.to_string(), "unsupported: no runtime yet");
        let raw = CallError::Rpc(RpcError {
            code: -32000,
            message: "x".into(),
            data: None,
        });
        assert_eq!(raw.to_string(), "code -32000: x");
    }

    #[test]
    fn calls_skip_and_keep_notifications() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut reader = BufReader::new(theirs.try_clone().unwrap());
            let mut writer = theirs;
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "hello");
            assert_eq!(request["params"]["role"]["kind"], "debug");
            let id = &request["id"];
            writer
                .write_all(
                    format!(
                        "{}\n{}\n",
                        r#"{"jsonrpc":"2.0","method":"machine.events","params":{"n":1}}"#,
                        serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"later","data":{"kind":"unsupported"}}})
                    )
                    .as_bytes(),
                )
                .unwrap();
        });
        let mut client = DaemonClient::new(ours).unwrap();
        let err = client.hello().unwrap_err();
        assert!(matches!(&err, CallError::Rpc(e) if e.data == Some(ErrorData::Unsupported)));
        assert_eq!(client.next_notification().unwrap().method, "machine.events");
        server.join().unwrap();
        assert!(matches!(client.next_notification(), Err(CallError::Closed)));
    }
}
