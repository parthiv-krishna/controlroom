//! The host `hello`.
//!
//! The params are the daemon API's [`crate::daemon::HelloParams`] (protocol version, client
//! name, [`crate::daemon::ClientRole`]), with the same single-controller rule: a host daemon
//! serves one controller at a time, a `hello` with the same `controller_id` replaces the old
//! connection, and debug clients connect alongside.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{DaemonId, Epoch};
use crate::version::{Capability, ProtocolVersion};

/// Result of the host `hello`. A major mismatch is answered with `incompatible_version`, a
/// second controller with `controller_conflict`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HostHelloResult {
    /// The `controlroomd` software version, e.g. `"0.1.0"`.
    pub host_version: String,
    /// The host protocol version it speaks (same major as the client's).
    pub protocol_version: ProtocolVersion,
    /// The host daemon installation's stable id.
    pub daemon_id: DaemonId,
    /// This daemon process's lifetime. A different epoch than on the previous connection means
    /// the daemon restarted: calls it was running without a supervisor are `lost`, and shells
    /// whose supervisor died reopen with a reset notice.
    pub epoch: Epoch,
    /// Optional features: `shells`, `pty`, `jobs`, `sandbox_approvals`; clients ignore unknown
    /// ones.
    pub capabilities: Vec<Capability>,
}
