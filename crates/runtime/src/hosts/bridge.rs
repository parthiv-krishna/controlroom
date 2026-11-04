//! The producer side of the `hostops` bridge: host requests waiting for the controller.
//!
//! A request stays pending until the controller answers it (`hostops.respond`) or the runtime
//! withdraws it. The controller pulls: `hostops.subscribe` returns every pending request, and
//! new requests and withdrawals follow as [`BridgeEvent`]s. Requests are deduplicated by id:
//! asking again for a pending id adds a waiter instead of a second request (a resent tool call).
//! The first response wins; later ones and responses to withdrawn requests are ignored.
//!
//! The bridge lives outside the runtime's core lock, so answering never waits for a transaction.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use controlroom_protocol::RpcError;
use controlroom_protocol::agentd::HostOpRequest;
use controlroom_protocol::ids::{HostOpId, SubscriptionId};
use controlroom_protocol::jsonrpc::RpcOutcome;
use serde_json::Value;
use tokio::sync::{broadcast, oneshot};

/// Bridge events buffered before a slow subscriber lags (it must resubscribe).
const EVENTS_CAPACITY: usize = 4096;

/// A change for the bridge's subscriber.
#[derive(Debug, Clone, PartialEq)]
pub enum BridgeEvent {
    /// A new request.
    Request(Box<HostOpRequest>),
    /// A request that is no longer needed.
    Withdraw(HostOpId),
}

/// The outcome of a host request: the host's result or error, verbatim.
pub(crate) type Outcome = Result<Value, RpcError>;

struct Pending {
    request: HostOpRequest,
    waiters: Vec<oneshot::Sender<Outcome>>,
    /// Order of arrival, so a subscribe lists requests oldest first.
    order: u64,
}

#[derive(Default)]
struct State {
    pending: BTreeMap<HostOpId, Pending>,
    next_order: u64,
}

/// Pending host requests and their subscriber.
pub(crate) struct Bridge {
    state: Mutex<State>,
    events: broadcast::Sender<BridgeEvent>,
}

impl Default for Bridge {
    fn default() -> Self {
        Bridge {
            state: Mutex::default(),
            events: broadcast::channel(EVENTS_CAPACITY).0,
        }
    }
}

impl Bridge {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `request` (or joins the pending one with its id) and returns its outcome.
    pub(crate) fn request(&self, request: HostOpRequest) -> oneshot::Receiver<Outcome> {
        let (tx, rx) = oneshot::channel();
        let mut state = self.lock();
        if let Some(pending) = state.pending.get_mut(&request.id) {
            pending.waiters.push(tx);
            return rx;
        }
        state.next_order += 1;
        let order = state.next_order;
        let id = request.id.clone();
        let _ = self
            .events
            .send(BridgeEvent::Request(Box::new(request.clone())));
        state.pending.insert(
            id,
            Pending {
                request,
                waiters: vec![tx],
                order,
            },
        );
        rx
    }

    /// Queues `request` with nobody waiting for its outcome.
    pub(crate) fn send(&self, request: HostOpRequest) {
        drop(self.request(request));
    }

    /// Whether a request with this id is pending.
    pub(crate) fn is_pending(&self, id: &HostOpId) -> bool {
        self.lock().pending.contains_key(id)
    }

    /// Drops a pending request; its waiters see their channel close.
    pub(crate) fn withdraw(&self, id: &HostOpId) {
        if self.lock().pending.remove(id).is_some() {
            let _ = self.events.send(BridgeEvent::Withdraw(id.clone()));
        }
    }

    /// Fails the pending requests that match `which` with `error` (their waiters get it) and
    /// withdraws them from the controller.
    pub(crate) fn fail(&self, which: impl Fn(&HostOpRequest) -> bool, error: &RpcError) {
        let failed: Vec<(HostOpId, Pending)> = {
            let mut state = self.lock();
            let ids: Vec<HostOpId> = state
                .pending
                .iter()
                .filter(|(_, p)| which(&p.request))
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| state.pending.remove(&id).map(|p| (id, p)))
                .collect()
        };
        for (id, pending) in failed {
            let _ = self.events.send(BridgeEvent::Withdraw(id));
            for waiter in pending.waiters {
                let _ = waiter.send(Err(error.clone()));
            }
        }
    }

    /// The controller's answer; `false` when nothing waited for it.
    pub(crate) fn respond(&self, id: &HostOpId, outcome: RpcOutcome<Value>) -> bool {
        let Some(pending) = self.lock().pending.remove(id) else {
            return false;
        };
        let outcome = match outcome {
            RpcOutcome::Result(value) => Ok(value),
            RpcOutcome::Error(error) => Err(error),
        };
        for waiter in pending.waiters {
            let _ = waiter.send(outcome.clone());
        }
        true
    }

    /// Every pending request, oldest first, and the events that follow.
    pub(crate) fn subscribe(
        &self,
    ) -> (
        SubscriptionId,
        Vec<HostOpRequest>,
        broadcast::Receiver<BridgeEvent>,
    ) {
        let state = self.lock();
        let events = self.events.subscribe();
        let mut pending: Vec<&Pending> = state.pending.values().collect();
        pending.sort_by_key(|p| p.order);
        let requests = pending.into_iter().map(|p| p.request.clone()).collect();
        (SubscriptionId::generate(), requests, events)
    }
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::host::{HostRequest, WorkspaceCloseParams};
    use controlroom_protocol::ids::{MachineId, WorkspaceId};

    use super::*;

    fn request(id: &str) -> HostOpRequest {
        HostOpRequest {
            id: HostOpId::new(id),
            machine_id: MachineId::generate(),
            session_id: None,
            request: HostRequest::WorkspaceClose(WorkspaceCloseParams {
                workspace_id: WorkspaceId::generate(),
            }),
        }
    }

    #[tokio::test]
    async fn requests_dedupe_first_response_wins_and_withdraw_closes() {
        let bridge = Bridge::default();
        let a = bridge.request(request("a"));
        let a2 = bridge.request(request("a"));
        let b = bridge.request(request("b"));
        let (_, pending, mut events) = bridge.subscribe();
        let ids: Vec<&str> = pending.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert!(bridge.respond(&HostOpId::new("a"), RpcOutcome::Result(Value::from(1))));
        assert!(!bridge.respond(&HostOpId::new("a"), RpcOutcome::Result(Value::from(2))));
        assert_eq!(a.await.unwrap().unwrap(), Value::from(1));
        assert_eq!(a2.await.unwrap().unwrap(), Value::from(1));
        bridge.withdraw(&HostOpId::new("b"));
        assert!(b.await.is_err());
        assert_eq!(
            events.recv().await.unwrap(),
            BridgeEvent::Withdraw(HostOpId::new("b"))
        );
    }
}
