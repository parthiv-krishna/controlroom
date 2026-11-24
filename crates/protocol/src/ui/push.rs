//! The Web Push payload: what the controller encrypts for a device and the service worker
//! (`web/sw/sw.js`) turns into a notification with a deep link.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{ProfileId, SessionId};

/// What a pushed session needs from the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PushState {
    /// A blocking approval or dialog.
    Approval,
    /// A non-blocking question.
    Question,
    /// A failed or interrupted run.
    Problem,
    /// A finished run with unread output.
    Finished,
    /// A state this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// What a notification may show, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PushContent {
    /// The session's name and machine (every profile except private ones).
    Session {
        /// The session's display name.
        name: String,
        /// The primary workspace's machine name.
        machine: String,
    },
    /// Metadata only, for a private profile: the notification's title is the profile's name and
    /// its body the attention kind ("Needs you"). No session name or text.
    Private {
        /// The profile's name.
        profile: String,
    },
    /// Content this build doesn't know: show a generic notification.
    #[serde(other)]
    Unknown,
}

/// The JSON payload of one push. On the wire the content's fields sit next to the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PushPayload {
    /// The session, for the deep link and as the notification's tag.
    pub session_id: SessionId,
    /// The session's profile, so the service worker can tag the notification; `null` from a
    /// controller before profiles.
    #[serde(default)]
    pub profile_id: Option<ProfileId>,
    /// What the session needs.
    pub state: PushState,
    /// The deep link, relative to the app root (`#/s/<id>`).
    pub url: String,
    /// What the notification shows.
    #[serde(flatten)]
    pub content: PushContent,
}
