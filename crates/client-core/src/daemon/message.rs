//! JSON-RPC 2.0 envelopes at the `serde_json` level.
//!
//! Only the envelope is interpreted here. Result, error `data` and
//! notification params stay [`Value`]s, so a body the client doesn't
//! understand (a newer daemon's error kind, a changed result type) can only
//! ever fail its own call, never the connection.

use serde::Deserialize;
use serde_json::{Map, Value};

use super::CallError;

/// A JSON-RPC error object returned by the daemon.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RpcError {
    /// The JSON-RPC error code.
    pub code: i64,
    /// A human-readable message.
    pub message: String,
    /// Structured details (the protocol's typed error kinds), undecoded.
    #[serde(default)]
    pub data: Option<Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

/// A notification from the daemon.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    /// The notification's method name.
    pub method: String,
    /// Its params (`null` if absent).
    pub params: Value,
}

/// One message read from the daemon.
#[derive(Debug)]
pub(super) enum Incoming {
    /// A response to the call with this id; a malformed body is already a
    /// [`CallError::Malformed`] for that call.
    Response {
        id: Value,
        outcome: Result<Value, CallError>,
    },
    Notification(Notification),
    /// A request from the daemon (the daemon API defines none).
    Request {
        id: Value,
        method: String,
    },
}

/// Classifies one line. Errors mean the line isn't a JSON-RPC message at all
/// (so no call can be blamed for it).
pub(super) fn parse_incoming(line: &[u8]) -> Result<Incoming, String> {
    let mut object: Map<String, Value> =
        serde_json::from_slice(line).map_err(|e| format!("not a JSON object: {e}"))?;
    let id = object.remove("id").filter(|id| !id.is_null());
    if let Some(method) = object.remove("method") {
        let Value::String(method) = method else {
            return Err("method is not a string".into());
        };
        return Ok(match id {
            Some(id) => Incoming::Request { id, method },
            None => Incoming::Notification(Notification {
                method,
                params: object.remove("params").unwrap_or(Value::Null),
            }),
        });
    }
    let id = id.ok_or("neither a method nor a response id")?;
    let outcome = match (object.remove("result"), object.remove("error")) {
        (_, Some(error)) => match serde_json::from_value::<RpcError>(error) {
            Ok(error) => Err(CallError::Rpc(error)),
            Err(e) => Err(CallError::Malformed(format!("error object: {e}"))),
        },
        (Some(result), None) => Ok(result),
        (None, None) => Err(CallError::Malformed(
            "response has neither result nor error".into(),
        )),
    };
    Ok(Incoming::Response { id, outcome })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(v: Value) -> Incoming {
        parse_incoming(v.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn classifies_messages() {
        let Incoming::Response { id, outcome } =
            parse(json!({"jsonrpc": "2.0", "id": 4, "result": {"a": 1}}))
        else {
            panic!()
        };
        assert_eq!((id, outcome.unwrap()), (json!(4), json!({"a": 1})));

        let Incoming::Notification(n) = parse(json!({"jsonrpc": "2.0", "method": "m"})) else {
            panic!()
        };
        assert_eq!((n.method.as_str(), n.params), ("m", Value::Null));

        let Incoming::Request { id, method } =
            parse(json!({"jsonrpc": "2.0", "id": "x", "method": "m", "params": {}}))
        else {
            panic!()
        };
        assert_eq!((id, method.as_str()), (json!("x"), "m"));
    }

    #[test]
    fn unknown_error_data_is_kept_undecoded() {
        let Incoming::Response { outcome, .. } = parse(json!({
            "jsonrpc": "2.0", "id": 1,
            "error": {"code": -32099, "message": "new", "data": {"kind": "from_the_future", "x": [1]}}
        })) else {
            panic!()
        };
        let Err(CallError::Rpc(e)) = outcome else {
            panic!("{outcome:?}")
        };
        assert_eq!(e.code, -32099);
        assert_eq!(e.data, Some(json!({"kind": "from_the_future", "x": [1]})));
    }

    #[test]
    fn malformed_bodies_fail_their_id() {
        for body in [
            json!({"jsonrpc": "2.0", "id": 2, "error": {"code": "busy"}}),
            json!({"jsonrpc": "2.0", "id": 2, "error": "busy"}),
            json!({"jsonrpc": "2.0", "id": 2}),
        ] {
            let Incoming::Response { id, outcome } = parse(body.clone()) else {
                panic!("{body}")
            };
            assert_eq!(id, json!(2));
            assert!(matches!(outcome, Err(CallError::Malformed(_))), "{body}");
        }
    }

    #[test]
    fn garbage_is_not_a_message() {
        for line in [&b"not json"[..], b"[1,2]", b"{}", b"{\"method\": 3}"] {
            assert!(parse_incoming(line).is_err());
        }
    }
}
