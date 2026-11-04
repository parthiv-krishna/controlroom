//! The host router: carries the agent daemon's host requests (`hostops.*`, protocol §5) to the
//! machines' host daemons and their answers and notifications back. It decides nothing about
//! calls.
//!
//! - **Pending requests** come from `hostops.subscribe` (every pending one, replacing what the
//!   router held), `hostops.request` (new ones) and `hostops.withdraw` (dropped ones). Each is
//!   sent to its machine as soon as that machine's host daemon is connected, and sent again
//!   on every new connection until it is answered: every host method is idempotent, so a
//!   response lost with a connection is simply asked for again.
//! - **Timeouts** are the host client's, except for `tool.run` and `tool.attach`, which wait as
//!   long as the command runs (the agent daemon applies the grace period instead).
//! - **Answers** go back verbatim with `hostops.respond`; a request for a machine the
//!   controller doesn't know (or that was removed) is answered `not_found {machine}`.
//! - **Ordering**: answers, forwarded host notifications (`hostops.notify`) and machine
//!   reports (`hostops.machines`) leave through one ordered [`outbound`] queue, so a call's
//!   output reaches the agent daemon before its result, and a new connection's report before
//!   its notifications.

mod forward;
pub(crate) mod outbound;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use controlroom_protocol::agentd::{HostOpRequest, HostOpsRespondParams};
use controlroom_protocol::error::Resource;
use controlroom_protocol::host::HostRequest;
use controlroom_protocol::ids::{HostOpId, MachineId, SubscriptionId};
use controlroom_protocol::ui::Connectivity;
use controlroom_protocol::{RpcError, RpcOutcome};
use serde_json::Value;
use tokio::task::AbortHandle;

use crate::errors::{invalid_params, not_found};
use crate::hub::{Hub, HubState, LinkKind};

/// A request on its way to (or with) its host.
#[derive(Debug)]
struct Pending {
    request: HostOpRequest,
    /// The forwarding task, while the request is with the host.
    flight: Option<Flight>,
}

#[derive(Debug)]
struct Flight {
    id: u64,
    task: AbortHandle,
}

/// The pending host requests, in arrival order.
#[derive(Debug, Default)]
pub(crate) struct Router {
    pending: BTreeMap<u64, Pending>,
    index: HashMap<HostOpId, u64>,
    next_seq: u64,
    next_flight: u64,
    /// The current `hostops.subscribe` subscription.
    subscription: Option<SubscriptionId>,
}

impl Router {
    fn insert(&mut self, request: HostOpRequest) -> u64 {
        self.next_seq += 1;
        let seq = self.next_seq;
        self.index.insert(request.id.clone(), seq);
        self.pending.insert(
            seq,
            Pending {
                request,
                flight: None,
            },
        );
        seq
    }

    fn remove(&mut self, id: &HostOpId) -> Option<HostOpRequest> {
        let seq = self.index.remove(id)?;
        let pending = self.pending.remove(&seq)?;
        if let Some(flight) = pending.flight {
            flight.task.abort();
        }
        Some(pending.request)
    }

    /// Whether a request is with `machine`'s host daemon right now.
    pub(crate) fn in_flight(&self, machine: MachineId) -> bool {
        self.pending
            .values()
            .any(|p| p.request.machine_id == machine && p.flight.is_some())
    }
}

/// The method and params of a host request, as sent on the wire.
fn wire(request: &HostRequest) -> Result<(&'static str, Value), RpcError> {
    let mut value =
        serde_json::to_value(request).map_err(|e| crate::errors::internal(e.to_string()))?;
    Ok((request.method(), value["params"].take()))
}

impl Hub {
    /// The agent daemon's `hostops.subscribe` result: replaces every pending request. Requests
    /// already with their host stay there.
    pub(crate) fn hostops_subscribed(
        self: &Arc<Self>,
        st: &mut HubState,
        subscription: SubscriptionId,
        requests: Vec<HostOpRequest>,
    ) {
        st.router.subscription = Some(subscription);
        let keep: std::collections::HashSet<_> = requests.iter().map(|r| r.id.clone()).collect();
        let dropped: Vec<_> = st
            .router
            .index
            .keys()
            .filter(|id| !keep.contains(*id))
            .cloned()
            .collect();
        for id in dropped {
            st.router.remove(&id);
        }
        for request in requests {
            if let Some(seq) = st.router.index.get(&request.id) {
                if let Some(pending) = st.router.pending.get_mut(seq) {
                    pending.request = request;
                }
                continue;
            }
            let seq = st.router.insert(request);
            self.dispatch(st, seq);
        }
    }

    /// `hostops.request`: new requests of the current subscription.
    pub(crate) fn hostops_requested(
        self: &Arc<Self>,
        st: &mut HubState,
        subscription: SubscriptionId,
        requests: Vec<HostOpRequest>,
    ) {
        if st.router.subscription != Some(subscription) {
            return;
        }
        for request in requests {
            if st.router.index.contains_key(&request.id) {
                continue;
            }
            let seq = st.router.insert(request);
            self.dispatch(st, seq);
        }
    }

    /// `hostops.withdraw`: drops requests (a forwarded one is abandoned; its answer, if any, is
    /// never sent).
    pub(crate) fn hostops_withdrawn(
        &self,
        st: &mut HubState,
        subscription: SubscriptionId,
        ids: &[HostOpId],
    ) {
        if st.router.subscription != Some(subscription) {
            return;
        }
        for id in ids {
            st.router.remove(id);
        }
    }

    /// The link to the agent daemon is gone. Requests stay with their hosts (an answer that
    /// arrives meanwhile is lost with the link and asked for again); the next subscription
    /// replaces them.
    pub(crate) fn hostops_reset(&self, st: &mut HubState) {
        st.router.subscription = None;
    }

    /// A machine's host daemon said hello: sends it every request still waiting for it.
    pub(crate) fn host_connected(self: &Arc<Self>, st: &mut HubState, machine: MachineId) {
        let waiting: Vec<u64> = st
            .router
            .pending
            .iter()
            .filter(|(_, p)| p.request.machine_id == machine && p.flight.is_none())
            .map(|(seq, _)| *seq)
            .collect();
        for seq in waiting {
            self.dispatch(st, seq);
        }
    }

    /// A machine's link is gone: its requests wait for the next connection. (Their calls fail
    /// with the connection anyway; aborting them here means the next connection can't race
    /// a late failure.)
    pub(crate) fn host_disconnected(&self, st: &mut HubState, machine: MachineId) {
        for pending in st.router.pending.values_mut() {
            if pending.request.machine_id == machine
                && let Some(flight) = pending.flight.take()
            {
                flight.task.abort();
            }
        }
    }

    /// A machine was removed: its requests are answered `not_found`.
    pub(crate) fn host_removed(&self, st: &mut HubState, machine: MachineId) {
        let ids: Vec<_> = st
            .router
            .pending
            .values()
            .filter(|p| p.request.machine_id == machine)
            .map(|p| p.request.id.clone())
            .collect();
        for id in ids {
            st.router.remove(&id);
            self.outbound.respond_now(HostOpsRespondParams {
                id,
                outcome: RpcOutcome::Error(not_found(Resource::Machine, machine)),
            });
        }
    }

    /// Sends request `seq` to its host if that host is connected; answers it at once if it
    /// can't ever be sent.
    fn dispatch(self: &Arc<Self>, st: &mut HubState, seq: u64) {
        let Some(pending) = st.router.pending.get(&seq) else {
            return;
        };
        let machine_id = pending.request.machine_id;
        let refusal = match st.machines.get(&machine_id) {
            None => Some(not_found(Resource::Machine, machine_id)),
            Some(_) if matches!(pending.request.request, HostRequest::Hello(_)) => Some(
                invalid_params("the controller owns the host handshake; hello is never forwarded"),
            ),
            Some(_) => None,
        };
        if let Some(error) = refusal {
            let id = pending.request.id.clone();
            st.router.remove(&id);
            self.outbound.respond_now(HostOpsRespondParams {
                id,
                outcome: RpcOutcome::Error(error),
            });
            return;
        }
        let Some(entry) = st.machines.get(&machine_id) else {
            return;
        };
        let client = match &entry.link {
            Some(link)
                if matches!(link.kind, LinkKind::Host { .. })
                    && entry.live.connectivity == Connectivity::Connected =>
            {
                link.daemon.clone()
            }
            _ => return,
        };
        let (method, params) = match wire(&pending.request.request) {
            Ok(wire) => wire,
            Err(e) => {
                tracing::error!(error = %e.message, "encoding a host request");
                return;
            }
        };
        st.router.next_flight += 1;
        let flight = st.router.next_flight;
        let task = tokio::spawn(forward::run(
            Arc::downgrade(self),
            client,
            pending.request.id.clone(),
            flight,
            method,
            params,
        ));
        if let Some(pending) = st.router.pending.get_mut(&seq) {
            pending.flight = Some(Flight {
                id: flight,
                task: task.abort_handle(),
            });
        }
    }

    /// A forwarded call ended. With an answer, the request is done (unless it was withdrawn or
    /// re-sent meanwhile) and the answer goes back; without one (the connection closed), it
    /// waits for the next connection. Returns the answer to send.
    pub(crate) fn hostops_finished(
        &self,
        id: &HostOpId,
        flight: u64,
        outcome: Option<RpcOutcome<Value>>,
    ) -> Option<HostOpsRespondParams> {
        let mut st = self.lock();
        let seq = *st.router.index.get(id)?;
        let pending = st.router.pending.get_mut(&seq)?;
        if pending.flight.as_ref().map(|f| f.id) != Some(flight) {
            return None;
        }
        let Some(outcome) = outcome else {
            pending.flight = None;
            return None;
        };
        st.router.index.remove(id);
        st.router.pending.remove(&seq);
        Some(HostOpsRespondParams {
            id: id.clone(),
            outcome,
        })
    }
}
