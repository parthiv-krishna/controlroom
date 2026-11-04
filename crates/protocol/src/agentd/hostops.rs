//! The host bridge (`hostops.*`): how the agent daemon reaches hosts without ever calling the
//! controller.
//!
//! The controller keeps sole custody of SSH keys and host trust, so it carries every host
//! request, pull-style:
//!
//! 1. The controller calls `hostops.subscribe` after each `hello`. The result lists **every**
//!    pending request; `hostops.request` notifications follow with new ones, and
//!    `hostops.withdraw` with ones the agent daemon no longer needs.
//! 2. The controller sends each request to its machine's host daemon when that machine is
//!    connected, and answers it with `hostops.respond` (the host's result or error, verbatim).
//!    A request whose host connection dropped before the response is sent again on the next
//!    connection: every host method is idempotent. A request for a machine the controller
//!    doesn't know is answered with `not_found` (`machine`). Duplicate responses are ignored
//!    (the first wins).
//! 3. The controller forwards every notification its host connections receive with
//!    `hostops.notify`, and reports its machines (names, availability, host epochs) with
//!    `hostops.machines` after `hello` and on every change.
//!
//! The agent daemon owns everything else: pending calls and their grace periods, journal acks,
//! job cursors, re-subscribing to jobs and approvals after each new host connection, and turning
//! host approvals and waits for input into session requests.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::host::{HostNotification, HostRequest};
use crate::ids::{Epoch, HostOpId, MachineId, SessionId, SubscriptionId};
use crate::jsonrpc::RpcOutcome;
use crate::session::Availability;

/// A host request to forward. On the wire the request's `method` and `params` sit next to the
/// routing fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct HostOpRequest {
    /// Identifies the request in `hostops.respond` and `hostops.withdraw`.
    pub id: HostOpId,
    /// The machine whose host daemon handles it.
    pub machine_id: MachineId,
    /// The session it serves, for logs; `null` for host-level requests (subscriptions, acks).
    pub session_id: Option<SessionId>,
    /// The host request (never `hello`: the controller owns the handshake).
    #[serde(flatten)]
    pub request: HostRequest,
}

/// Result of `hostops.subscribe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct HostOpsSubscribeResult {
    /// Identifies this subscription.
    pub subscription_id: SubscriptionId,
    /// Every pending request, oldest first. Replaces whatever the controller held from an
    /// earlier subscription.
    pub requests: Vec<HostOpRequest>,
}

/// Params of the `hostops.request` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct HostOpsRequestParams {
    /// The subscription.
    pub subscription_id: SubscriptionId,
    /// New requests, in order.
    pub requests: Vec<HostOpRequest>,
}

/// Params of the `hostops.withdraw` notification: drop these requests without sending them
/// (one already sent may still be answered; the agent daemon ignores the response).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HostOpsWithdrawParams {
    /// The subscription.
    pub subscription_id: SubscriptionId,
    /// The requests.
    pub ids: Vec<HostOpId>,
}

/// Params of `hostops.respond`: the host's answer to a request. On the wire `result` or
/// `error` sits next to `id`, as in a JSON-RPC response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct HostOpsRespondParams {
    /// The request.
    pub id: HostOpId,
    /// The host's result (typed by the request's method) or error.
    #[serde(flatten)]
    pub outcome: RpcOutcome<Value>,
}

/// Params of `hostops.notify`: a notification from a host daemon, forwarded verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct HostOpsNotifyParams {
    /// The machine whose host daemon sent it.
    pub machine_id: MachineId,
    /// The notification.
    pub notification: HostNotification,
}

/// A machine as the agent daemon needs to know it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HostMachine {
    /// The machine.
    pub machine_id: MachineId,
    /// Its display name, which the agent uses in `machine:path` (unique, no `:`).
    pub name: String,
    /// Whether it can be reached. A new `since` while connected means a new host connection:
    /// subscriptions on the host must be renewed.
    pub availability: Availability,
    /// The host daemon's epoch from the latest `hello`; `null` before the first one. A change
    /// means the host daemon restarted.
    pub epoch: Option<Epoch>,
    /// How long a tool call waits for this machine before it fails with `connection_lost`
    /// (default 600).
    pub grace_s: u32,
}

/// Params of `hostops.machines`: the controller's machines, replacing the previous list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HostOpsMachinesParams {
    /// Every machine.
    pub machines: Vec<HostMachine>,
}
