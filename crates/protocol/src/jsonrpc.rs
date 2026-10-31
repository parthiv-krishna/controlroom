//! Typed JSON-RPC 2.0 envelopes, newline-delimited framing, and the [`Method`] /
//! [`NotificationMethod`] traits that tie each method name to its params and result types.
//!
//! **Framing:** every message is one compact JSON object followed by `\n`. Compact JSON never
//! contains a raw newline, so a reader splits on `\n`. Batches (JSON arrays) are not used.
//!
//! **Tolerance:** [`Message::parse`] classifies a line by shape and keeps params and results as
//! raw JSON. A response whose `result` doesn't match the method's result type fails only that
//! call ([`RpcResponse::into_typed`]); a malformed `error` member still resolves its id
//! ([`RpcError::from_value_lenient`]); unknown members are ignored. A line that can't be parsed
//! at all reports the request id when it is readable ([`MessageError::id`]), so a client can
//! fail that one call instead of waiting for its timeout.
//!
//! **Timeouts** are client-side: every call has one (30 s by default; longer for methods known
//! to be slow, such as `machines.add_confirm`, `doctor.report` and `fs.list` on a slow host). On
//! expiry the client fails the call with [`ErrorData::Timeout`] and drops a late response.
//!
//! JSON-RPC carries control traffic only. Bulk or interactive bytes (terminals, file transfer)
//! travel on separate channels, so nothing here assumes it is the only channel between peers.

use std::fmt;

use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use ts_rs::TS;

use crate::error::{ErrorData, RpcError};

/// The `"jsonrpc": "2.0"` marker. Serializes as `"2.0"` and rejects anything else.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct JsonRpcVersion;

impl Serialize for JsonRpcVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let version = String::deserialize(deserializer)?;
        if version == "2.0" {
            Ok(JsonRpcVersion)
        } else {
            Err(D::Error::custom(format!(
                "unsupported jsonrpc version {version:?}"
            )))
        }
    }
}

/// A JSON-RPC request id, echoed back verbatim in the response.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum RpcId {
    /// A numeric id, within JavaScript's safe integer range.
    Number(i64),
    /// A string id.
    String(String),
}

impl fmt::Display for RpcId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RpcId::Number(n) => write!(f, "{n}"),
            RpcId::String(s) => write!(f, "{s:?}"),
        }
    }
}

impl From<i64> for RpcId {
    fn from(id: i64) -> Self {
        RpcId::Number(id)
    }
}

impl From<&str> for RpcId {
    fn from(id: &str) -> Self {
        RpcId::String(id.to_owned())
    }
}

/// A request: a method call that expects exactly one [`RpcResponse`] with the same id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct RpcRequest<P> {
    /// Always `"2.0"`.
    #[ts(type = "\"2.0\"")]
    pub jsonrpc: JsonRpcVersion,
    /// Chosen by the caller and echoed in the response.
    pub id: RpcId,
    /// The method name, e.g. `"session.prompt"`.
    pub method: String,
    /// The method's params. Always an object in both controlroom APIs.
    pub params: P,
}

impl<P> RpcRequest<P> {
    /// Builds a request for method `M`.
    pub fn new<M>(id: impl Into<RpcId>, params: P) -> Self
    where
        M: Method<Params = P>,
    {
        RpcRequest {
            jsonrpc: JsonRpcVersion,
            id: id.into(),
            method: M::NAME.to_owned(),
            params,
        }
    }
}

/// A notification: a one-way message with no id and no response. Used for subscription feeds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct RpcNotification<P> {
    /// Always `"2.0"`.
    #[ts(type = "\"2.0\"")]
    pub jsonrpc: JsonRpcVersion,
    /// The notification name, e.g. `"session.events"`.
    pub method: String,
    /// The notification's params.
    pub params: P,
}

impl<P> RpcNotification<P> {
    /// Builds a notification for `N`.
    pub fn new<N>(params: P) -> Self
    where
        N: NotificationMethod<Params = P>,
    {
        RpcNotification {
            jsonrpc: JsonRpcVersion,
            method: N::NAME.to_owned(),
            params,
        }
    }
}

/// The outcome half of an [`RpcResponse`]: exactly one of `result` or `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub enum RpcOutcome<R> {
    /// The call succeeded.
    #[serde(rename = "result")]
    Result(R),
    /// The call failed.
    #[serde(rename = "error")]
    Error(RpcError),
}

/// A response to an [`RpcRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct RpcResponse<R> {
    /// Always `"2.0"`.
    #[ts(type = "\"2.0\"")]
    pub jsonrpc: JsonRpcVersion,
    /// The id of the request this answers; `null` only when the request's id was unreadable.
    pub id: Option<RpcId>,
    /// `result` or `error`.
    #[serde(flatten)]
    pub outcome: RpcOutcome<R>,
}

impl<R> RpcResponse<R> {
    /// A successful response.
    pub fn success(id: RpcId, result: R) -> Self {
        RpcResponse {
            jsonrpc: JsonRpcVersion,
            id: Some(id),
            outcome: RpcOutcome::Result(result),
        }
    }

    /// An error response. `id` is `None` only if the request's id could not be read.
    pub fn error(id: Option<RpcId>, error: RpcError) -> Self {
        RpcResponse {
            jsonrpc: JsonRpcVersion,
            id,
            outcome: RpcOutcome::Error(error),
        }
    }

    /// Converts the outcome into a `Result`.
    pub fn into_result(self) -> Result<R, RpcError> {
        match self.outcome {
            RpcOutcome::Result(result) => Ok(result),
            RpcOutcome::Error(error) => Err(error),
        }
    }
}

impl RpcResponse<Value> {
    /// Decodes an untyped response as method `M`'s result. A result that doesn't parse becomes
    /// an [`ErrorData::InvalidResponse`] error for this call only.
    pub fn into_typed<M: Method>(self) -> Result<M::Result, RpcError> {
        let value = self.into_result()?;
        serde_json::from_value(value).map_err(|err| {
            RpcError::new(
                ErrorData::InvalidResponse,
                format!("malformed {} result: {err}", M::NAME),
            )
        })
    }
}

/// The empty result of commands whose effect shows up in the subscription feeds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Ack {}

/// The empty params of methods that take none. Clients may send `{}` or omit `params`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NoParams {}

/// Ties a request method name to its params and result types. Implemented by the zero-sized
/// marker types in [`crate::daemon::methods`] and [`crate::ui::methods`].
pub trait Method {
    /// The JSON-RPC method name.
    const NAME: &'static str;
    /// The `params` object.
    type Params: Serialize + DeserializeOwned + TS + 'static;
    /// The `result` of a successful call.
    type Result: Serialize + DeserializeOwned + TS + 'static;
}

/// Ties a notification name to its params type. Implemented by the marker types in
/// [`crate::daemon::notifications`] and [`crate::ui::notifications`].
pub trait NotificationMethod {
    /// The JSON-RPC method name of the notification.
    const NAME: &'static str;
    /// The `params` object.
    type Params: Serialize + DeserializeOwned + TS + 'static;
}

/// Parses `params` for method or notification `name`, treating omitted or `null` params as an
/// empty object. Errors map to `invalid_params`.
pub fn parse_params<T: DeserializeOwned>(name: &str, params: Value) -> Result<T, RpcError> {
    let params = match params {
        Value::Null => Value::Object(Map::new()),
        other => other,
    };
    serde_json::from_value(params).map_err(|err| {
        RpcError::new(
            ErrorData::InvalidParams,
            format!("invalid params for {name}: {err}"),
        )
    })
}

/// Any incoming JSON-RPC message, classified by shape. Params and results stay raw JSON until
/// the receiver decodes them for the specific method.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Has `method` and `id`.
    Request(RpcRequest<Value>),
    /// Has `method` but no `id`.
    Notification(RpcNotification<Value>),
    /// Has `result` or `error`.
    Response(RpcResponse<Value>),
}

/// Why a line isn't a usable JSON-RPC message.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{error}")]
pub struct MessageError {
    /// The message's id, when it could be read. A server answers the error to it; a client
    /// fails the pending call with this id.
    pub id: Option<RpcId>,
    /// `parse_error` for invalid JSON, `invalid_request` for JSON that isn't JSON-RPC 2.0.
    pub error: Box<RpcError>,
}

impl Message {
    /// Parses one line (with or without its trailing newline).
    pub fn parse(line: &str) -> Result<Message, MessageError> {
        let value: Value = serde_json::from_str(line.trim_end_matches(['\n', '\r']))
            .map_err(|err| invalid(None, ErrorData::ParseError, err.to_string()))?;
        let Value::Object(mut obj) = value else {
            return Err(invalid(
                None,
                ErrorData::InvalidRequest,
                "not a JSON object",
            ));
        };

        let id = match obj.remove("id") {
            None | Some(Value::Null) => None,
            Some(raw) => Some(serde_json::from_value::<RpcId>(raw).map_err(|_| {
                invalid(
                    None,
                    ErrorData::InvalidRequest,
                    "id is not a number or string",
                )
            })?),
        };
        if obj.get("jsonrpc") != Some(&Value::String("2.0".into())) {
            return Err(invalid(
                id,
                ErrorData::InvalidRequest,
                "jsonrpc must be \"2.0\"",
            ));
        }

        match obj.remove("method") {
            Some(Value::String(method)) => {
                let params = obj.remove("params").unwrap_or(Value::Null);
                Ok(match id {
                    Some(id) => Message::Request(RpcRequest {
                        jsonrpc: JsonRpcVersion,
                        id,
                        method,
                        params,
                    }),
                    None => Message::Notification(RpcNotification {
                        jsonrpc: JsonRpcVersion,
                        method,
                        params,
                    }),
                })
            }
            Some(_) => Err(invalid(
                id,
                ErrorData::InvalidRequest,
                "method is not a string",
            )),
            None => {
                let outcome = match (obj.remove("error"), obj.remove("result")) {
                    (Some(error), _) if !error.is_null() => {
                        RpcOutcome::Error(RpcError::from_value_lenient(error))
                    }
                    (_, Some(result)) => RpcOutcome::Result(result),
                    _ => {
                        return Err(invalid(
                            id,
                            ErrorData::InvalidRequest,
                            "neither method, result nor error",
                        ));
                    }
                };
                Ok(Message::Response(RpcResponse {
                    jsonrpc: JsonRpcVersion,
                    id,
                    outcome,
                }))
            }
        }
    }
}

fn invalid(id: Option<RpcId>, data: ErrorData, message: impl Into<String>) -> MessageError {
    MessageError {
        id,
        error: Box::new(RpcError::new(data, message)),
    }
}

/// Serializes `message` as one newline-terminated line.
pub fn encode_line<T: Serialize>(message: &T) -> serde_json::Result<String> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    Ok(line)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn classifies_messages() {
        let req = Message::parse(r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{}}"#);
        assert!(matches!(req, Ok(Message::Request(r)) if r.method == "hello"));
        let note = Message::parse(r#"{"jsonrpc":"2.0","method":"session.events"}"#).unwrap();
        assert!(matches!(note, Message::Notification(n) if n.params.is_null()));
        let ok = Message::parse(r#"{"jsonrpc":"2.0","id":"a","result":null}"#).unwrap();
        assert!(matches!(ok, Message::Response(r) if r.outcome == RpcOutcome::Result(Value::Null)));
        let err = Message::parse(
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad"}}"#,
        )
        .unwrap();
        assert!(
            matches!(err, Message::Response(r) if r.id.is_none() && matches!(r.outcome, RpcOutcome::Error(_)))
        );
    }

    #[test]
    fn bad_envelopes_report_their_id_when_readable() {
        let parse = Message::parse("{not json").unwrap_err();
        assert_eq!((parse.id, parse.error.code), (None, -32700));
        let version = Message::parse(r#"{"jsonrpc":"1.0","id":7,"method":"x"}"#).unwrap_err();
        assert_eq!(
            (version.id, version.error.code),
            (Some(RpcId::Number(7)), -32600)
        );
        let empty = Message::parse(r#"{"jsonrpc":"2.0","id":"q"}"#).unwrap_err();
        assert_eq!(empty.id, Some(RpcId::String("q".into())));
    }

    #[test]
    fn malformed_error_member_still_resolves_its_id() {
        let line = r#"{"jsonrpc":"2.0","id":4,"error":"exploded"}"#;
        let Ok(Message::Response(resp)) = Message::parse(line) else {
            panic!("expected a response");
        };
        assert_eq!(resp.id, Some(RpcId::Number(4)));
        let err = resp.into_result().unwrap_err();
        assert_eq!(err.data, Some(ErrorData::InvalidResponse));
    }

    #[test]
    fn responses_flatten_result_or_error() {
        let ok = RpcResponse::success(RpcId::Number(3), json!({"a": 1}));
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            json!({"jsonrpc": "2.0", "id": 3, "result": {"a": 1}})
        );
        let line = encode_line(&ok).unwrap();
        assert!(line.ends_with('\n') && !line.trim_end().contains('\n'));
    }

    #[test]
    fn omitted_params_parse_as_empty_object() {
        assert!(parse_params::<NoParams>("x", Value::Null).is_ok());
        let err = parse_params::<u32>("x", json!({})).unwrap_err();
        assert_eq!(err.code, -32602);
    }
}
