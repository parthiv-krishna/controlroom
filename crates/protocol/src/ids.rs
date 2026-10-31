//! Identifier, cursor, timestamp and remote-path newtypes shared by both APIs.
//!
//! All of these serialize as plain JSON strings or numbers, so the wire stays simple and the
//! generated TypeScript gets one named alias per concept (`export type SessionId = string;`).
//!
//! Two kinds of ids exist:
//! - **ULID ids** are assigned by a store (the daemon's or the controller's) and validated on
//!   decode: a malformed one is a decode error.
//! - **Opaque string ids** are chosen by someone who can't or shouldn't mint ULIDs: clients
//!   ([`OpId`], [`DeviceId`]) or the daemon's deterministic transcript fold ([`ItemId`],
//!   [`RequestId`], which must come out the same on every re-fold).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use ulid::Ulid;

macro_rules! ulid_id {
    ($(#[doc = $doc:literal])* $Name:ident) => {
        $(#[doc = $doc])*
        ///
        /// A ULID, serialized as its 26-character canonical string.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
        pub struct $Name(#[ts(type = "string")] pub Ulid);

        impl $Name {
            /// Generates a fresh id from the current time and a random component.
            pub fn generate() -> Self {
                Self(Ulid::generate())
            }
        }

        impl From<Ulid> for $Name {
            fn from(ulid: Ulid) -> Self {
                Self(ulid)
            }
        }

        impl fmt::Display for $Name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $Name {
            type Err = ulid::DecodeError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ulid::from_string(s).map(Self)
            }
        }
    };
}

macro_rules! string_id {
    ($(#[doc = $doc:literal])* $Name:ident) => {
        $(#[doc = $doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
        pub struct $Name(pub String);

        impl $Name {
            /// Wraps an id string as-is.
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// The id as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $Name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $Name {
            fn from(id: &str) -> Self {
                Self(id.to_owned())
            }
        }

        impl From<String> for $Name {
            fn from(id: String) -> Self {
                Self(id)
            }
        }
    };
}

// ---------------------------------------------------------------------------------------------
// Daemon-assigned ids
// ---------------------------------------------------------------------------------------------

ulid_id! {
    /// A controlroom session, assigned by the daemon at `session.create`. Stable forever, and
    /// unique per daemon.
    SessionId
}
ulid_id! {
    /// One daemon installation (one Unix user on one host), generated on its first start and
    /// reported by the daemon `hello`. Daemon-side references to sessions on *other* machines
    /// use it (see [`crate::session::PeerRef`]); the controller maps it to its [`MachineId`].
    DaemonId
}
ulid_id! {
    /// One queued prompt in a session's queue.
    QueueId
}
ulid_id! {
    /// One entry of a session's shared todo list.
    TodoId
}
ulid_id! {
    /// A background job (a supervised process). Its log is `<cwd>/.controlroom/jobs/<id>.log`.
    JobId
}
ulid_id! {
    /// A grant: a sandbox grant on the daemon or a contact grant on the controller.
    GrantId
}
ulid_id! {
    /// A mailbox message, shared by the sender's outbox item and the receiver's inbox delivery
    /// so replies can refer to it.
    MessageId
}
ulid_id! {
    /// One item in a daemon's durable outbox (`outbox.subscribe`, `outbox.ack`).
    OutboxId
}
ulid_id! {
    /// A subscription handle returned by a `*.subscribe` method and echoed in each of its
    /// notifications. Scoped to one connection.
    SubscriptionId
}

// ---------------------------------------------------------------------------------------------
// Controller-assigned ids
// ---------------------------------------------------------------------------------------------

ulid_id! {
    /// One controller installation, sent in the daemon `hello` so the daemon can record which
    /// controller is connected (a daemon serves one controller at a time).
    ControllerId
}
ulid_id! {
    /// The controller's machine record. Not the same thing as the daemon's own [`DaemonId`].
    MachineId
}
ulid_id! {
    /// An additional SSH key stored by the controller.
    KeyId
}
ulid_id! {
    /// A project: a saved view over sessions and tasks.
    ProjectId
}
ulid_id! {
    /// A task (a kanban item).
    TaskId
}
ulid_id! {
    /// One checklist entry of a task.
    ChecklistItemId
}
ulid_id! {
    /// A schedule that wakes one session with a canned message.
    ScheduleId
}
ulid_id! {
    /// The lifetime of one controller state feed. A new epoch invalidates every [`Rev`] of the
    /// previous one (see [`crate::ui::StateCursor`]).
    Epoch
}

// ---------------------------------------------------------------------------------------------
// Opaque string ids
// ---------------------------------------------------------------------------------------------

string_id! {
    /// The client-chosen idempotency key of a mutating command.
    ///
    /// The daemon stores op ids in the same transaction as the command's effect; repeating a
    /// command with the same op id returns the original result instead of applying it twice.
    /// Clients normally use a ULID ([`OpId::generate`]), but any unique string works: the
    /// controller's scheduler uses `<schedule id>:<scheduled unix ms>` so a retry can never
    /// double-fire. The UI API passes op ids through to the daemon unchanged.
    OpId
}

impl OpId {
    /// A fresh op id (a ULID string).
    pub fn generate() -> Self {
        Self(Ulid::generate().to_string())
    }
}

string_id! {
    /// A browser's persistent device id, generated by the browser and kept in its local
    /// storage. Any unique string up to 64 characters (a ULID or UUID).
    DeviceId
}
string_id! {
    /// A timeline item. Assigned by the daemon's transcript fold, deterministically, so a
    /// re-fold yields the same ids. Unique within one session.
    ItemId
}
string_id! {
    /// A request (approval, question or dialog) within one session. Assigned by the daemon,
    /// deterministically from the transcript; never the agent's raw JSON-RPC id.
    RequestId
}

// ---------------------------------------------------------------------------------------------
// Positions and time
// ---------------------------------------------------------------------------------------------

/// A position in a session transcript. Records are numbered from 1, so `Seq(0)` means "before
/// the first record".
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS,
)]
pub struct Seq(pub u64);

/// A position in a revisioned feed: the daemon's machine feed and outbox, or the controller's
/// state feed. Revisions start at 1, so `Rev(0)` means "before the first change".
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS,
)]
pub struct Rev(pub u64);

/// A point in time as milliseconds since the Unix epoch (UTC). In TypeScript a `number`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS,
)]
pub struct UnixMillis(pub i64);

impl UnixMillis {
    /// The current time.
    pub fn now() -> Self {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        Self(millis)
    }
}

// ---------------------------------------------------------------------------------------------
// Remote paths
// ---------------------------------------------------------------------------------------------

/// A path on an execution machine: UTF-8, `/`-separated, and it may start with `~` (the remote
/// user's home), which only the daemon expands. Never a `std::path::Path`, because the
/// controller's platform says nothing about the remote one. On the wire a plain string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
pub struct RemotePath(pub String);

impl RemotePath {
    /// Wraps a remote path string as-is.
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    /// The path as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the path starts at the filesystem root (`/`).
    pub fn is_absolute(&self) -> bool {
        self.0.starts_with('/')
    }

    /// Appends `/`-separated components, with exactly one `/` between the parts.
    pub fn join(&self, rest: &str) -> Self {
        let rest = rest.trim_start_matches('/');
        if self.0.is_empty() {
            return Self(rest.to_owned());
        }
        if rest.is_empty() {
            return self.clone();
        }
        match self.0.trim_end_matches('/') {
            "" => Self(format!("/{rest}")),
            base => Self(format!("{base}/{rest}")),
        }
    }
}

impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for RemotePath {
    fn from(path: &str) -> Self {
        Self(path.to_owned())
    }
}

impl From<String> for RemotePath {
    fn from(path: String) -> Self {
        Self(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulid_ids_are_validated_strings() {
        let id = SessionId::generate();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<SessionId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<SessionId>("\"not-a-ulid\"").is_err());
    }

    #[test]
    fn string_ids_accept_any_string() {
        let op: OpId = serde_json::from_str("\"sched:1700000000000\"").unwrap();
        assert_eq!(op.as_str(), "sched:1700000000000");
        assert_eq!(OpId::generate().as_str().len(), 26);
    }

    #[test]
    fn cursors_are_numbers() {
        assert_eq!(serde_json::to_string(&Seq(7)).unwrap(), "7");
        assert_eq!(serde_json::to_string(&Rev(3)).unwrap(), "3");
        assert_eq!(
            serde_json::to_string(&UnixMillis(1_700_000_000_000)).unwrap(),
            "1700000000000"
        );
    }

    #[test]
    fn remote_path_join_uses_forward_slashes() {
        assert_eq!(RemotePath::from("~/src").join("app").as_str(), "~/src/app");
        assert_eq!(RemotePath::from("/").join("etc").as_str(), "/etc");
        assert_eq!(
            RemotePath::from("/srv/").join("/data/").as_str(),
            "/srv/data/"
        );
        assert_eq!(RemotePath::from("").join("x").as_str(), "x");
        assert_eq!(RemotePath::from("/a").join("").as_str(), "/a");
        assert!(RemotePath::from("/home/alice").is_absolute());
        assert!(!RemotePath::from("~/src").is_absolute());
    }
}
