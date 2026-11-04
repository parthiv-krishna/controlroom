//! Prompt content and the references used for attribution: devices, peers and authors.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{DaemonId, DeviceId, ScheduleId, SessionId};

/// One part of a prompt or message. Neutral: drivers translate to and from the agent's format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text (Markdown for agent messages).
    Text {
        /// The text.
        text: String,
    },
    /// An inline image.
    Image {
        /// The MIME type, e.g. `"image/png"`.
        mime_type: String,
        /// The image bytes, base64-encoded.
        data: String,
    },
    /// A reference to a resource (usually a file on the session's machine).
    Resource {
        /// The resource URI, e.g. `file:///home/alice/src/app/README.md`.
        uri: String,
        /// A display name.
        name: Option<String>,
        /// The MIME type, when known.
        mime_type: Option<String>,
    },
    /// A part kind this build doesn't know.
    #[serde(other)]
    Unknown,
}

impl ContentPart {
    /// A text part.
    pub fn text(text: impl Into<String>) -> Self {
        ContentPart::Text { text: text.into() }
    }
}

/// A device acting on the user's behalf, with its label at that time. The UI shows the device's
/// current label (from the device list) and falls back to `label` once the device is removed.
/// Attribution always names the device, never "you".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
pub struct DeviceRef {
    /// The device.
    pub device_id: DeviceId,
    /// Its label when it acted, e.g. `"Laptop (Firefox on Linux)"`.
    pub label: String,
}

/// A session on any machine, as the daemon refers to it: by the session's [`DaemonId`]. The UI
/// maps the daemon id to a machine with [`crate::ui::Machine::daemon_id`].
///
/// With the agent daemon every session is local to it, so `daemon_id` is always the agent
/// daemon's own id; the mailbox rework (R3) is expected to reduce peers to session ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
pub struct PeerRef {
    /// The daemon the session lives on.
    pub daemon_id: DaemonId,
    /// The session.
    pub session_id: SessionId,
    /// The session's display name when the reference was made.
    pub name: String,
}

/// A session's relation to a peer it may contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PeerRelation {
    /// Linked to the same task.
    Task,
    /// The parent session.
    Parent,
    /// A child session.
    Child,
    /// A user-approved contact grant.
    Grant,
    /// A relation this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// One entry of a session's contact directory, maintained by the controller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Peer {
    /// The peer session.
    pub peer: PeerRef,
    /// Why it may be contacted.
    pub relation: PeerRelation,
    /// The shared task's title, for `task` peers.
    pub task: Option<String>,
}

/// Who changed something that both the agent and the user can edit (todos, for example).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Author {
    /// The session's agent.
    Agent,
    /// The user, from a device (absent when no device was registered).
    User {
        /// The acting device.
        device: Option<DeviceRef>,
    },
    /// The daemon itself (e.g. an item skipped by a policy).
    System,
    /// An author kind this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// Where a prompt sent with `session.prompt` comes from. Shown as the run's trigger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptOrigin {
    /// Typed by the user. The controller fills in the device of the authenticated connection
    /// and replaces any value a client sent.
    User {
        /// The sending device.
        device: Option<DeviceRef>,
    },
    /// Fired by a controller schedule. The agent sees a normal prompt.
    Schedule {
        /// The schedule.
        schedule_id: ScheduleId,
        /// Its name when it fired, shown as `schedule: <name>`.
        name: String,
    },
    /// An origin this build doesn't know.
    #[serde(other)]
    Unknown,
}

impl Default for PromptOrigin {
    fn default() -> Self {
        PromptOrigin::User { device: None }
    }
}
