//! Terminal streams: browser ↔ controller, one WebSocket per attached terminal (spec §22).
//!
//! `GET /api/stream/terminal/<session id>/<shell name>` (the shell name URL-encoded) upgrades to
//! a WebSocket under the same Origin, Host and login rules as the UI API's `/api/ws`. The
//! controller runs `controlroomd pty attach` on the shell's machine (see
//! [`crate::host::terminal`]); several browsers may attach to one shell, and any of them types.
//!
//! - **Binary frames** carry raw terminal bytes: input from the browser, output to it.
//! - **Text frames** carry one JSON object: [`TerminalCommand`] from the browser,
//!   [`TerminalEvent`] to it.
//!
//! The stream starts with `attached {reset: true}` and the replay of recent output. When the
//! link to the machine drops, the controller sends `detached` and keeps the socket: once the
//! machine is back it sends `attached {reset: false}` and continues where the output left off
//! (or `reset: true` with a fresh replay when that isn't possible). Input typed while detached
//! is dropped.
//!
//! **Close codes:** 1000 after `ended` (the shell's process ended, or the shell or session is
//! gone); 4401 when the login is gone (logout, device or login revoked, allow-list change);
//! 1001 when the server shuts down; 1008 for an invalid frame.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A text frame from the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalCommand {
    /// The terminal's size in the browser; the last resize from any browser wins.
    Resize {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// A command this build doesn't know (ignored).
    #[serde(other)]
    Unknown,
}

/// A text frame to the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalEvent {
    /// Output follows. With `reset`, it is a replay of recent output: clear the terminal first.
    Attached {
        /// Clear the terminal before writing the output that follows.
        reset: bool,
    },
    /// The link to the shell's machine dropped; output resumes when it is back.
    Detached {
        /// Why, for display.
        reason: String,
    },
    /// The terminal's foreground program changed (e.g. `"vim"`); `null` at the shell's prompt.
    /// It decides whether Ctrl-h/j/k/l go to the terminal (spec §13).
    Foreground {
        /// The program's name.
        name: Option<String>,
    },
    /// The stream ended (the shell's process ended, or the shell is gone); the socket closes.
    Ended {
        /// Why, for display.
        reason: String,
    },
    /// An event this build doesn't know; ignore it.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wire_shapes() {
        assert_eq!(
            serde_json::to_value(TerminalEvent::Attached { reset: true }).unwrap(),
            json!({"kind": "attached", "reset": true})
        );
        assert_eq!(
            serde_json::to_value(TerminalEvent::Foreground { name: None }).unwrap(),
            json!({"kind": "foreground", "name": null})
        );
        assert_eq!(
            serde_json::from_value::<TerminalCommand>(
                json!({"kind": "resize", "cols": 80, "rows": 24})
            )
            .unwrap(),
            TerminalCommand::Resize { cols: 80, rows: 24 }
        );
        assert_eq!(
            serde_json::from_value::<TerminalCommand>(json!({"kind": "paste", "x": 1})).unwrap(),
            TerminalCommand::Unknown
        );
        assert_eq!(
            serde_json::from_value::<TerminalEvent>(json!({"kind": "bell"})).unwrap(),
            TerminalEvent::Unknown
        );
    }
}
