//! The control API of a long-running mock model (`controlroom-fixture-tool mock-model`).
//!
//! Tests that can't hold a [`MockModel`](super::MockModel) in-process (the NixOS VM
//! end-to-end, where pi runs on another machine than the test script) script it over HTTP on
//! the same loopback port as the model:
//!
//! ```text
//! POST /control/replies        [ReplySpec, ...]   append scripted replies
//! POST /control/release/<gate>                    let streams waiting at <gate> continue
//! GET  /control/state                             {"remaining", "requests", "reached"}
//! GET  /control/requests                          the request bodies received, in order
//! ```

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Reply, Shared, lock};

/// A scripted reply as JSON, e.g. `{"text": "hi"}` or
/// `{"tool_call": {"id": "c1", "name": "bash", "args": {"command": "ls"}}}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ReplySpec {
    /// [`Reply::text`].
    Text(String),
    /// [`Reply::thinking`].
    Thinking { thinking: String, text: String },
    /// [`Reply::tool_call`].
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
    /// [`Reply::gated_text`].
    GatedText {
        before: String,
        gate: String,
        after: String,
    },
    /// [`Reply::error`].
    Error { status: u16, message: String },
}

impl From<ReplySpec> for Reply {
    fn from(spec: ReplySpec) -> Self {
        match spec {
            ReplySpec::Text(text) => Reply::text(&text),
            ReplySpec::Thinking { thinking, text } => Reply::thinking(&thinking, &text),
            ReplySpec::ToolCall { id, name, args } => Reply::tool_call(&id, &name, args),
            ReplySpec::GatedText {
                before,
                gate,
                after,
            } => Reply::gated_text(&before, &gate, &after),
            ReplySpec::Error { status, message } => Reply::error(status, &message),
        }
    }
}

/// What `GET /control/state` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlState {
    /// Scripted replies not consumed yet.
    pub remaining: usize,
    /// Model requests received so far.
    pub requests: usize,
    /// Gates a stream has reached, released or not.
    pub reached: Vec<String>,
}

/// The `/control/*` routes.
pub(super) fn routes() -> axum::Router<Arc<Shared>> {
    axum::Router::new()
        .route("/control/replies", post(append))
        .route("/control/release/{gate}", post(release))
        .route("/control/state", get(state))
        .route("/control/requests", get(requests))
}

async fn append(State(shared): State<Arc<Shared>>, Json(specs): Json<Vec<ReplySpec>>) {
    lock(&shared.replies).extend(specs.into_iter().map(Reply::from));
}

async fn release(State(shared): State<Arc<Shared>>, Path(gate): Path<String>) {
    shared.released.send_modify(|set| {
        set.insert(gate);
    });
}

async fn state(State(shared): State<Arc<Shared>>) -> Json<ControlState> {
    Json(ControlState {
        remaining: lock(&shared.replies).len(),
        requests: lock(&shared.requests).len(),
        reached: shared.reached.borrow().iter().cloned().collect(),
    })
}

async fn requests(State(shared): State<Arc<Shared>>) -> Json<Vec<Value>> {
    Json(lock(&shared.requests).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reply_specs_parse() {
        let specs: Vec<ReplySpec> = serde_json::from_value(json!([
            {"text": "hi"},
            {"thinking": {"thinking": "hm", "text": "ok"}},
            {"tool_call": {"id": "c1", "name": "bash", "args": {"command": "ls"}}},
            {"gated_text": {"before": "a", "gate": "g", "after": "b"}},
            {"error": {"status": 503, "message": "busy"}},
        ]))
        .unwrap();
        assert_eq!(specs.len(), 5);
        assert!(matches!(
            Reply::from(specs[4].clone()),
            Reply::Error { status: 503, .. }
        ));
        assert!(serde_json::from_value::<ReplySpec>(json!({"text": "a", "extra": 1})).is_err());
        assert!(serde_json::from_value::<ReplySpec>(json!({"sing": "a"})).is_err());
    }
}
