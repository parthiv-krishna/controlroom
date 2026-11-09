//! Typed JSON-RPC errors shared by both APIs.
//!
//! Every error a controlroom peer sends carries `data.kind`, a stable `snake_case` string with
//! kind-specific detail fields, so clients switch on `error.data.kind` rather than on `message`.
//! The numeric `code` follows JSON-RPC: `-32700..=-32600` for envelope errors, `-32000` and
//! below for application errors. The UI API passes daemon errors through unchanged.
//!
//! **Tolerance:** an error kind this build doesn't know (from a newer peer), or a known kind
//! whose detail fields don't parse, decodes as [`ErrorData::Unknown`]. The `code` and `message`
//! are always kept, so the error still reaches the caller of that request id.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::ids::{ControllerId, MachineId, RemotePath, SessionId, UnixMillis};
use crate::session::{Request, SessionState};
use crate::ui::Connectivity;
use crate::version::ProtocolVersion;

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, thiserror::Error)]
#[error("{message} (code {code})")]
pub struct RpcError {
    /// Numeric JSON-RPC code; see [`ErrorData::code`].
    pub code: i32,
    /// Human-readable description, for logs and as a fallback in the UI.
    pub message: String,
    /// The typed error. Always present in errors sent by controlroom peers.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_data"
    )]
    pub data: Option<ErrorData>,
}

impl RpcError {
    /// Builds an error whose `code` matches `data`.
    pub fn new(data: ErrorData, message: impl Into<String>) -> Self {
        RpcError {
            code: data.code(),
            message: message.into(),
            data: Some(data),
        }
    }

    /// The typed error, if the peer sent one.
    pub fn data(&self) -> Option<&ErrorData> {
        self.data.as_ref()
    }

    /// Decodes an `error` member leniently: anything that isn't a JSON-RPC error object becomes
    /// an `invalid_response` error, so a response is never lost because its error is malformed.
    pub fn from_value_lenient(value: Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_else(|_| {
            RpcError::new(
                ErrorData::InvalidResponse,
                format!("malformed error object: {value}"),
            )
        })
    }
}

/// `data` parses as `Unknown` when its kind is unknown or its fields don't match the kind.
fn lenient_data<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<ErrorData>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(
        serde_json::from_value(value).unwrap_or(ErrorData::Unknown),
    ))
}

/// What a `not_found` error refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    /// A queue item.
    QueueItem,
    /// A request of a session.
    Request,
    /// A todo entry.
    Todo,
    /// A background job.
    Job,
    /// A sandbox or contact grant.
    Grant,
    /// An agent key not in the machine's `agents.toml`.
    Agent,
    /// A subscription on this connection.
    Subscription,
    /// An outbox item.
    OutboxItem,
    /// A session option (`session.set_option`).
    Option,
    /// A machine record (UI API).
    Machine,
    /// An additional SSH key (UI API).
    Key,
    /// A project (UI API).
    Project,
    /// A task (UI API).
    Task,
    /// A task checklist entry (UI API).
    ChecklistItem,
    /// A device (UI API).
    Device,
    /// A schedule (UI API).
    Schedule,
    /// A session's workspace (agent daemon and host APIs), or a machine that isn't one of the
    /// session's workspaces (agent API).
    Workspace,
    /// A tool call no host journal knows (`tool.attach`).
    Call,
    /// A host sandbox approval.
    Approval,
    /// A model provider (`models.*`).
    Provider,
    /// A subscription sign-in (`models.login_*`).
    SignIn,
    /// A resource kind this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// The typed error kinds, serialized with a `kind` tag. Kinds marked "UI API" are only produced
/// by the controller; kinds marked "Host API" by host daemons (and passed through to the agent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ErrorData {
    /// The message was not valid JSON.
    ParseError,
    /// The message was JSON but not a valid JSON-RPC 2.0 message.
    InvalidRequest,
    /// No such method (possibly one this peer's minor version doesn't have).
    MethodNotFound,
    /// The params didn't match the method's params type.
    InvalidParams,
    /// An unexpected failure on the serving side.
    InternalError,
    /// The peers' protocol majors differ.
    IncompatibleVersion {
        /// The protocol version the responding side speaks.
        protocol_version: ProtocolVersion,
        /// The responding side's software version.
        version: String,
    },
    /// A method other than `hello` was called before the handshake.
    HelloRequired,
    /// The `op_id` was already used for a *different* command. Repeating the same command with
    /// the same `op_id` is not an error: it returns the original result.
    OpIdConflict,
    /// Client-side: no response arrived within the call's timeout. The UI API also returns it
    /// when the controller's call to a daemon timed out.
    Timeout,
    /// Client-side: a response arrived for this id but its body didn't parse. Only that call
    /// fails; the connection stays up.
    InvalidResponse,
    /// The method or option isn't available here (e.g. a feature the peer didn't advertise).
    Unsupported,
    /// The referenced object doesn't exist.
    NotFound {
        /// What kind of object.
        resource: Resource,
        /// The id that was looked up, as a string.
        id: String,
    },
    /// No session with this id (never existed, or deleted by `session.delete`).
    UnknownSession {
        /// The requested session.
        id: SessionId,
    },
    /// The session was purged by the retention policy.
    Deleted {
        /// The purged session.
        id: SessionId,
    },
    /// The request was already resolved: the first decision wins. Carries the winning
    /// resolution.
    AlreadyResolved {
        /// The request with its winning answer.
        request: Box<Request>,
    },
    /// The session is in the middle of a conflicting operation (e.g. starting); retry later.
    Busy,
    /// The command doesn't apply in the session's current state (e.g. `session.prompt` on a
    /// closed session).
    InvalidState {
        /// The session's state when the command was rejected.
        state: SessionState,
    },
    /// The agent session can't be reattached; offer "start fresh".
    NotResumable,
    /// The agent is configured but its dependency check failed.
    AgentUnavailable {
        /// The agent key.
        agent: String,
        /// Why, e.g. "pi: command not found".
        detail: String,
    },
    /// The session's sandbox can't be set up (nono missing, Landlock unavailable, profile
    /// rejected).
    SandboxUnavailable {
        /// Why.
        detail: String,
    },
    /// A filesystem operation failed (e.g. `fs.list` on an unreadable directory).
    FsError {
        /// The path, as given by the caller.
        path: RemotePath,
        /// The OS error description.
        detail: String,
    },
    /// A `hello` from a controller while a *different* controller is connected: a daemon serves
    /// one controller at a time. (The same `controller_id` replaces its old connection.)
    ControllerConflict {
        /// The connected controller.
        controller_id: ControllerId,
        /// When it connected.
        connected_since: UnixMillis,
    },
    /// UI API: the request was not authenticated (or the login session was revoked). Agent API:
    /// the `hello` token is wrong.
    Unauthenticated,
    /// UI API: the machine's daemon can't be reached, so a pass-through command was not
    /// delivered. Agent daemon API: a workspace's host can't be reached, so a session can't be
    /// created on it (or the workspace added).
    MachineUnavailable {
        /// The machine.
        machine_id: MachineId,
        /// Its current connectivity.
        connectivity: Box<Connectivity>,
    },
    /// UI API: the host presented a different key than the pinned or confirmed one.
    HostKeyMismatch {
        /// The expected fingerprint.
        expected: String,
        /// The fingerprint the host presented.
        actual: String,
    },
    /// UI API: the host is not Linux x86_64 or aarch64.
    UnsupportedPlatform {
        /// The `uname -sm` output.
        uname: String,
    },
    /// UI API: the key is still used by machines and can't be deleted.
    KeyInUse {
        /// Machines that use the key.
        machine_ids: Vec<MachineId>,
    },
    /// UI API: an imported private key could not be parsed or decrypted.
    InvalidKey {
        /// What went wrong, e.g. "wrong passphrase".
        detail: String,
    },
    /// Host API: a write's `expect_sha256` didn't match because the file changed since it was
    /// read. Nothing was written; read the file again and retry.
    Conflict {
        /// The file, resolved on the host.
        path: RemotePath,
    },
    /// Host API: the host daemon restarted while the call ran and its result is gone. Never
    /// rerun automatically.
    Lost {
        /// The call may have (partly) run, e.g. a command whose shell died with the daemon.
        /// `false` when it surely had no effect (a write whose file doesn't hold the new
        /// content), so retrying is safe.
        may_have_run: bool,
    },
    /// Host API: the shell doesn't exist (never opened, or closed).
    ShellGone {
        /// The shell's name.
        shell: String,
    },
    /// Agent API: the call's host stayed unreachable past the grace period. The call may still
    /// be running there; its result is injected into the session when the host is back.
    ConnectionLost {
        /// The unreachable machine.
        machine_id: MachineId,
        /// Since when it has been unreachable.
        since: UnixMillis,
    },
    /// An error kind this build doesn't know, or a known kind with malformed fields. The
    /// error's `code` and `message` are still available.
    #[serde(other)]
    Unknown,
}

impl ErrorData {
    /// The numeric JSON-RPC error code for this kind.
    pub fn code(&self) -> i32 {
        match self {
            ErrorData::ParseError => -32700,
            ErrorData::InvalidRequest => -32600,
            ErrorData::MethodNotFound => -32601,
            ErrorData::InvalidParams => -32602,
            ErrorData::InternalError => -32603,
            ErrorData::Unknown => -32000,
            ErrorData::IncompatibleVersion { .. } => -32001,
            ErrorData::HelloRequired => -32002,
            ErrorData::OpIdConflict => -32003,
            ErrorData::Timeout => -32004,
            ErrorData::InvalidResponse => -32005,
            ErrorData::Unsupported => -32006,
            ErrorData::NotFound { .. } => -32007,
            ErrorData::UnknownSession { .. } => -32010,
            ErrorData::Deleted { .. } => -32011,
            ErrorData::AlreadyResolved { .. } => -32012,
            ErrorData::Busy => -32013,
            ErrorData::InvalidState { .. } => -32014,
            ErrorData::NotResumable => -32015,
            ErrorData::AgentUnavailable { .. } => -32016,
            ErrorData::SandboxUnavailable { .. } => -32017,
            ErrorData::FsError { .. } => -32018,
            ErrorData::ControllerConflict { .. } => -32019,
            ErrorData::Unauthenticated => -32100,
            ErrorData::MachineUnavailable { .. } => -32101,
            ErrorData::HostKeyMismatch { .. } => -32102,
            ErrorData::UnsupportedPlatform { .. } => -32103,
            ErrorData::KeyInUse { .. } => -32104,
            ErrorData::InvalidKey { .. } => -32105,
            ErrorData::Conflict { .. } => -32020,
            ErrorData::Lost { .. } => -32021,
            ErrorData::ShellGone { .. } => -32022,
            ErrorData::ConnectionLost { .. } => -32023,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn error_carries_kind_and_matching_code() {
        let err = RpcError::new(ErrorData::NotResumable, "agent cannot resume");
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!({"code": -32015, "message": "agent cannot resume", "data": {"kind": "not_resumable"}})
        );
    }

    #[test]
    fn unknown_or_malformed_data_is_kept_as_unknown() {
        let newer: RpcError = serde_json::from_value(
            json!({"code": -32050, "message": "quota", "data": {"kind": "over_quota", "limit": 3}}),
        )
        .unwrap();
        assert_eq!(newer.data, Some(ErrorData::Unknown));
        assert_eq!(newer.code, -32050);

        let malformed: RpcError = serde_json::from_value(
            json!({"code": -32012, "message": "x", "data": {"kind": "already_resolved"}}),
        )
        .unwrap();
        assert_eq!(malformed.data, Some(ErrorData::Unknown));

        let bare: RpcError =
            serde_json::from_value(json!({"code": -32603, "message": "x", "data": null})).unwrap();
        assert_eq!(bare.data, None);
    }

    #[test]
    fn non_object_error_becomes_invalid_response() {
        let err = RpcError::from_value_lenient(json!("boom"));
        assert_eq!(err.data, Some(ErrorData::InvalidResponse));
    }
}
