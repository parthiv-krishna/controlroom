//! The per-connection ordering contract.
//!
//! Requests are read in arrival order. Each gets an ordering key: requests with the same key
//! run one at a time, in arrival order; requests with different keys (or none) run
//! concurrently. Keys:
//!
//! - a session-scoped command (`machine_id` and `id`): that session;
//! - any other machine-scoped command: that machine;
//! - the handshake, heartbeat and the slow SSH flows (`machines.add`, `machines.add_confirm`,
//!   `keys.rotate`): none;
//! - everything else (controller data, subscriptions, focus): one shared key.
//!
//! So two edits of the same session's queue, or a task create followed by a task move, are
//! applied in the order they were sent, while a slow command on one machine never holds up
//! another machine.

use std::collections::HashMap;
use std::sync::Mutex;

use controlroom_protocol::ids::{MachineId, SessionId};
use serde_json::Value;
use tokio::sync::oneshot;

/// What a request is ordered with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum OrderKey {
    Session(MachineId, SessionId),
    Machine(MachineId),
    Local,
}

/// The ordering key of a request; `None` runs it unordered.
pub(crate) fn order_key(method: &str, params: &Value) -> Option<OrderKey> {
    if matches!(
        method,
        "hello" | "ping" | "machines.add" | "machines.add_confirm" | "keys.rotate"
    ) {
        return None;
    }
    let text = |field: &str| params.get(field)?.as_str();
    let machine: Option<MachineId> = text("machine_id").and_then(|s| s.parse().ok());
    let session: Option<SessionId> = text("id").and_then(|s| s.parse().ok());
    Some(match (machine, session) {
        (Some(m), Some(s)) => OrderKey::Session(m, s),
        (Some(m), None) => OrderKey::Machine(m),
        _ => OrderKey::Local,
    })
}

/// Hands out turns per key in arrival order.
#[derive(Default)]
pub(crate) struct Sequencer {
    tails: Mutex<HashMap<OrderKey, oneshot::Receiver<()>>>,
}

/// One request's place in its key's line; dropping it lets the next one run.
pub(crate) struct Turn {
    previous: Option<oneshot::Receiver<()>>,
    _done: Option<oneshot::Sender<()>>,
}

impl Turn {
    /// Waits until every earlier request with the same key has finished.
    pub(crate) async fn ready(&mut self) {
        if let Some(previous) = self.previous.take() {
            // The sender is dropped (never sent) when the previous request ends.
            let _ = previous.await;
        }
    }
}

impl Sequencer {
    /// Takes the next turn for `key`; call in arrival order.
    pub(crate) fn enter(&self, key: Option<OrderKey>) -> Turn {
        let Some(key) = key else {
            return Turn {
                previous: None,
                _done: None,
            };
        };
        let (done, next) = oneshot::channel();
        let mut tails = self.tails.lock().unwrap_or_else(|p| p.into_inner());
        let previous = tails.insert(key, next);
        Turn {
            previous,
            _done: Some(done),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    #[test]
    fn keys_follow_the_params() {
        let m = MachineId::generate();
        let s = SessionId::generate();
        assert_eq!(
            order_key("queue.edit", &json!({"machine_id": m, "id": s})),
            Some(OrderKey::Session(m, s))
        );
        assert_eq!(
            order_key("fs.list", &json!({"machine_id": m, "path": "~"})),
            Some(OrderKey::Machine(m))
        );
        assert_eq!(order_key("tasks.move", &json!({})), Some(OrderKey::Local));
        assert_eq!(order_key("machines.add_confirm", &json!({})), None);
    }

    #[tokio::test]
    async fn same_key_runs_in_arrival_order() {
        let seq = Sequencer::default();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for i in 0..5 {
            let mut turn = seq.enter(Some(OrderKey::Local));
            let log = log.clone();
            handles.push(tokio::spawn(async move {
                // Later requests are ready sooner; the turn still orders them.
                tokio::task::yield_now().await;
                turn.ready().await;
                log.lock().unwrap().push(i);
                if i == 0 {
                    tokio::task::yield_now().await;
                }
                drop(turn);
            }));
        }
        // An unordered request never waits.
        seq.enter(None).ready().await;
        for h in handles.into_iter().rev() {
            h.await.unwrap();
        }
        assert_eq!(*log.lock().unwrap(), [0, 1, 2, 3, 4]);
    }
}
