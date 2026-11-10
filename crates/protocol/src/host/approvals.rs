//! Sandbox approvals raised on a host (`approvals.subscribe`, `approval.request`,
//! `approval.closed`, `approval.respond`).
//!
//! nono asks the host daemon's loopback webhook (one URL and secret per workspace); the host
//! turns the question into an approval card and holds nono's request open. The agent daemon,
//! subscribed through the controller, shows it as a blocking `sandbox` request on the
//! workspace's session and sends the user's choice back with `approval.respond`. The first
//! resolution wins; nono's timeout, nono hanging up or a host restart close it as `expired`
//! (a denial).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{ApprovalId, SessionId, SubscriptionId, UnixMillis, WorkspaceId};
use crate::session::{RequestBody, RequestOption, RequestState};

/// The option ids of host approvals ([`HostApproval::options`]).
pub mod options {
    /// Allow this one request (commands and endpoints).
    pub const ALLOW_ONCE: &str = "allow-once";
    /// Allow; nono remembers it for the rest of its run (file access).
    pub const ALLOW_SESSION: &str = "allow-session";
    /// Allow now, and add a standing grant with the requested access to the machine's sandbox
    /// defaults (file access). The host also allows it for the rest of the workspace's life.
    pub const ALLOW_ALWAYS: &str = "allow-always";
    /// Deny now, and add the path to the machine's "never" list: later requests for it are
    /// denied without asking (file access).
    pub const NEVER: &str = "never";
    /// Deny.
    pub const DENY: &str = "deny";
}

/// A pending sandbox approval, with everything needed to show and answer it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HostApproval {
    /// The approval.
    pub id: ApprovalId,
    /// The session whose workspace asked.
    pub session_id: SessionId,
    /// The workspace whose sandbox asked.
    pub workspace_id: WorkspaceId,
    /// One-line summary, e.g. `"Run git push origin main?"`.
    pub title: String,
    /// Longer explanation (Markdown).
    pub detail: Option<String>,
    /// The structured subject: `git_push`, `command`, `path` or `endpoint`.
    pub body: RequestBody,
    /// The answers ([`options`]): `allow-once` / `deny` for commands and endpoints;
    /// `allow-session` / `allow-always` / `never` / `deny` for file access.
    pub options: Vec<RequestOption>,
    /// When nono asked.
    pub created_at: UnixMillis,
    /// The user's own (sandboxed) terminal asked, not the agent.
    #[serde(default)]
    pub from_user_terminal: bool,
}

/// Result of `approvals.subscribe`: the pending approvals, then `approval.request` and
/// `approval.closed` notifications. Only the controller may subscribe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApprovalsSubscribeResult {
    /// Identifies this subscription.
    pub subscription_id: SubscriptionId,
    /// Approvals waiting for an answer, oldest first.
    pub pending: Vec<HostApproval>,
}

/// Params of the `approval.request` notification: a new approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApprovalRequestParams {
    /// The subscription.
    pub subscription_id: SubscriptionId,
    /// The approval.
    pub approval: HostApproval,
}

/// Params of the `approval.closed` notification: an approval ended without an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApprovalClosedParams {
    /// The subscription.
    pub subscription_id: SubscriptionId,
    /// The approval.
    pub approval_id: ApprovalId,
    /// `expired` (nono's timeout, nono hung up, or a host restart) or `withdrawn` (its
    /// workspace closed). Either is a denial.
    pub state: RequestState,
}

/// Params of `approval.respond`. The first resolution wins; answering an approval that is no
/// longer pending is `not_found` (`approval`), since the host keeps no closed approvals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApprovalRespondParams {
    /// The approval.
    pub approval_id: ApprovalId,
    /// The chosen [`RequestOption::id`].
    pub option_id: String,
    /// A denial's reason, passed to the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
