//! Wire types of the supervisor socket protocol (documented in the crate docs).

use std::path::PathBuf;

use controlroom_transcript::{ExitInfo, ProcessKind, Record};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The protocol version this supervisor speaks. Version 2 added terminals (`write_bytes`,
/// `resize`).
pub const PROTOCOL_VERSION: u32 = 2;

/// The oldest supervisor protocol version clients accept (N-1, but at least 1).
/// Supervisors outlive daemon upgrades, so a client must talk to older ones.
pub const MIN_PROTOCOL_VERSION: u32 = if PROTOCOL_VERSION > 1 {
    PROTOCOL_VERSION - 1
} else {
    1
};

/// Version of this crate (and of the `controlroomd` binary it is built into).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What a foreground supervisor prints to stdout once, when ready or on failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadyLine {
    /// The socket is listening and the child is running.
    Ready(SupervisorInfo),
    /// Startup failed.
    Error(String),
}

/// Describes a running supervisor: the ready line, and `supervisor.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorInfo {
    /// The supervised process id.
    pub id: String,
    /// What kind of process it supervises.
    pub kind: ProcessKind,
    /// Supervisor pid.
    pub pid: u32,
    /// Child pid, which is also its process group id.
    pub child_pid: u32,
    /// The socket path.
    pub socket: PathBuf,
    /// The transcript path.
    pub transcript: PathBuf,
    /// Supervisor protocol version.
    pub protocol_version: u32,
    /// Version of the supervisor binary.
    pub version: String,
    /// Unix ms.
    pub started_at: u64,
}

/// A request: `{"id": <any JSON>, "method": "...", "params": {...}}`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Request {
    #[serde(default)]
    pub id: Value,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

/// Any message from the supervisor: a result, an error, or a streamed record.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Message {
    #[serde(default)]
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<Record>,
}

/// `hello` parameters.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct HelloParams {
    pub protocol_version: u32,
    #[serde(default)]
    pub writer: bool,
}

/// `hello` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The supervisor's protocol version.
    pub protocol_version: u32,
    /// Version of the supervisor binary.
    pub version: String,
    /// The supervised process id.
    pub id: String,
    /// What kind of process it supervises.
    pub kind: ProcessKind,
    /// Supervisor pid.
    pub pid: u32,
    /// Child pid (and process group id).
    pub child_pid: u32,
    /// The transcript path.
    pub transcript: PathBuf,
    /// Whether this connection holds the writer role.
    pub writer: bool,
}

/// `read` parameters.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct ReadParams {
    #[serde(default)]
    pub from_seq: u64,
}

/// `read` result.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ReadResult {
    pub last_seq: u64,
}

/// `write` parameters.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct WriteParams {
    pub line: String,
}

/// `write` result.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct WriteResult {
    pub seq: u64,
}

/// `write_bytes` parameters.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct WriteBytesParams {
    /// The bytes, base64-encoded.
    pub data: String,
}

/// `resize` parameters.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ResizeParams {
    pub cols: u16,
    pub rows: u16,
}

/// The result of `write_bytes` and `resize`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Done {}

/// `terminate` parameters.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct TerminateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_ms: Option<u64>,
}

/// `terminate` result.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TerminateResult {
    pub already_exited: bool,
}

/// `sync` and `barrier` result.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DurableResult {
    pub durable_seq: u64,
}

/// `status` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The supervised process id.
    pub id: String,
    /// What kind of process it supervises.
    pub kind: ProcessKind,
    /// Supervisor pid.
    pub pid: u32,
    /// Child pid (and process group id).
    pub child_pid: u32,
    /// False as soon as the child has been reaped.
    pub alive: bool,
    /// Set once the exit record is written.
    pub exit: Option<ExitInfo>,
    /// Seq of the last record in the transcript.
    pub last_seq: u64,
    /// Every record up to this seq is fsynced.
    pub durable_seq: u64,
    /// Whether a connection holds the writer role.
    pub writer_attached: bool,
    /// The supervisor's protocol version.
    pub protocol_version: u32,
    /// Version of the supervisor binary.
    pub version: String,
}

/// An error response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ErrorBody {
    /// Machine-readable code.
    pub code: ErrorCode,
    /// Human-readable detail.
    pub message: String,
}

impl ErrorBody {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Error codes; `snake_case` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request was not valid JSON (the response id is null).
    ParseError,
    /// The params didn't fit the method.
    InvalidParams,
    /// The method doesn't exist in this protocol version.
    UnknownMethod,
    /// `hello` must come first.
    HelloRequired,
    /// Another connection holds the writer role.
    WriterBusy,
    /// The method needs the writer role.
    NotWriter,
    /// The child has exited; nothing more can be written.
    ChildExited,
    /// The child's stdin is `/dev/null`.
    NoStdin,
    /// The line exceeds the spawn spec's `max_line_bytes`.
    LineTooLong,
    /// The transcript could not be written or fsynced.
    Io,
    /// A code this version doesn't know.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_tolerate_unknown_values() {
        let body: ErrorBody =
            serde_json::from_str(r#"{"code":"rate_limited","message":"x"}"#).unwrap();
        assert_eq!(body.code, ErrorCode::Unknown);
        let body: ErrorBody =
            serde_json::from_str(r#"{"code":"line_too_long","message":"x"}"#).unwrap();
        assert_eq!(body.code, ErrorCode::LineTooLong);
    }

    #[test]
    fn messages_ignore_unknown_fields() {
        let msg: Message =
            serde_json::from_str(r#"{"id":3,"result":{"seq":4,"extra":true},"later":1}"#).unwrap();
        let result: WriteResult = serde_json::from_value(msg.result.unwrap()).unwrap();
        assert_eq!(result.seq, 4);
    }
}
