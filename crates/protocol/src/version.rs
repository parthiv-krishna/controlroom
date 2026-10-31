//! Protocol versions, the compatibility rule and `hello` capabilities.

use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A `major.minor` protocol version, exchanged in both `hello` handshakes.
///
/// **Compatibility rule:** peers are compatible iff their `major` versions are equal. A minor
/// version only *adds*, and every addition is something an older peer can ignore or avoid:
///
/// - new methods and notifications (a client checks the peer's minor version or a
///   [`Capability`] before calling a new method; an older server answers `method_not_found`);
/// - new optional params fields and new result fields (unknown fields are ignored);
/// - new enum variants, including error kinds, timeline item kinds and request kinds (an older
///   receiver decodes them as the enum's `unknown` variant).
///
/// Anything else (removing or renaming a field, changing a type or a method's meaning) is a
/// major change. A major mismatch puts the machine in the `incompatible` connectivity state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
pub struct ProtocolVersion {
    /// Incremented for breaking changes; peers with different majors are incompatible.
    pub major: u32,
    /// Incremented for additions.
    pub minor: u32,
}

impl ProtocolVersion {
    /// Whether peers speaking `self` and `other` can talk: their majors match.
    pub fn is_compatible_with(self, other: ProtocolVersion) -> bool {
        self.major == other.major
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The daemon API version (controller ↔ `controlroomd`), sent in the daemon `hello`.
pub const DAEMON_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

/// The UI API version (browser ↔ controller), sent in the UI `hello`. Versioned separately from
/// the daemon API: the UI ships with its controller, while daemons upgrade independently.
pub const UI_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

/// An optional feature a peer advertises in `hello.capabilities`.
///
/// Capabilities are plain strings, so a peer keeps (and can show) ones it doesn't know. A
/// method that belongs to a feature must only be called when the peer advertised it; otherwise
/// it answers `unsupported` or `method_not_found`. Later minor versions add features such as
/// `terminals` and `files` the same way.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
pub struct Capability(#[ts(type = "string")] pub Cow<'static, str>);

impl Capability {
    /// Background jobs: `job.*` and job timeline items (daemon).
    pub const JOBS: Capability = Capability(Cow::Borrowed("jobs"));
    /// The shared todo list and auto-continue: `todo.*` (daemon).
    pub const TODOS: Capability = Capability(Cow::Borrowed("todos"));
    /// Subagents as child sessions (daemon).
    pub const SUBAGENTS: Capability = Capability(Cow::Borrowed("subagents"));
    /// The durable mailbox: `outbox.*` and `inbox.deliver` (daemon).
    pub const MAILBOX: Capability = Capability(Cow::Borrowed("mailbox"));
    /// Schedules: `schedules.*` (controller).
    pub const SCHEDULES: Capability = Capability(Cow::Borrowed("schedules"));

    /// Wraps any capability string.
    pub fn new(name: impl Into<String>) -> Self {
        Capability(Cow::Owned(name.into()))
    }

    /// The capability string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_major_mismatch_is_incompatible() {
        let v = |major, minor| ProtocolVersion { major, minor };
        assert!(v(1, 0).is_compatible_with(v(1, 3)));
        assert!(!v(1, 0).is_compatible_with(v(2, 0)));
        assert_eq!(v(1, 2).to_string(), "1.2");
    }

    #[test]
    fn capabilities_are_strings_and_keep_unknown_values() {
        assert_eq!(
            serde_json::to_string(&Capability::JOBS).unwrap(),
            "\"jobs\""
        );
        let caps: Vec<Capability> = serde_json::from_str(r#"["jobs","teleport"]"#).unwrap();
        assert_eq!(caps[0], Capability::JOBS);
        assert_eq!(caps[1].as_str(), "teleport");
    }
}
