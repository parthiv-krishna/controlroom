//! The controller: client store, machine manager, hub and event bus, UI API, attention and
//! push. The web shell (`crates/web`) wraps it in HTTP.
//!
//! # Shell-facing API
//!
//! **Start and stop.** [`Controller::start`] with a [`ControllerConfig`]: the SQLite store
//! path, the required [`MasterKey`] (from the systemd credential), the bundled daemons
//! directory (`CONTROLROOM_BUNDLED_DAEMONS_DIR`), the server version, the [`AuthPolicy`]
//! (mode plus OIDC allow-list), optional Web Push ([`PushConfig::web_push`] with the VAPID key
//! credential file), the key comment and [`Tuning`]. It must run inside a Tokio runtime; it
//! starts every machine. [`Controller::shutdown`] closes every connection and stops the
//! machine tasks.
//!
//! **Authentication.** The shell verifies the login (OIDC callback, or the dev token on
//! loopback) and passes an [`Identity`] to [`Controller::resolve`], which applies the
//! allow-list and returns a [`Principal`] (v1: every admitted identity is the one owner,
//! [`OwnerId::local`]). [`Controller::create_login`] stores a login session tagged with the
//! auth mode, subject and groups and returns the cookie token and CSRF token
//! ([`NewLogin`]); only the token's hash is stored. On **every HTTP request**, call
//! [`Controller::authenticate`] with the cookie token: it slides the expiry and re-checks the
//! mode and allow-list ([`Authenticated`]). [`Controller::logout`],
//! [`Controller::revoke_device`] and [`Controller::revoke_owner`] revoke login sessions **and
//! close the affected open connections**.
//!
//! **UI connections.** For each WebSocket, [`Controller::open_client`] returns a [`UiClient`]:
//! pass every text frame from the browser to [`UiClient::send`] (it waits while the
//! connection is at its concurrency cap, so read the socket in the same task), and write
//! every frame from [`UiClient::recv`] to the socket ([`UiClient::split`] gives two halves
//! for separate tasks). `recv` returning `None` means the controller closed the connection;
//! [`UiClient::close_reason`] says why ([`CloseReason::Unauthenticated`]: sign in again;
//! [`CloseReason::Overflow`]: the tab fell behind and should reconnect;
//! [`CloseReason::Shutdown`]). Dropping the client (or both halves) ends its subscriptions and
//! focus. Every request re-checks the login session and allow-list, and frames are one
//! JSON-RPC message each (no batches, no newlines).
//!
//! **Push.** The browser subscribes through the UI API (`notifications.subscribe`);
//! [`Controller::subscribe_push`] does the same for an HTTP endpoint if the shell prefers one.
//! [`Controller::vapid_public_key`] is the browser's `applicationServerKey`.
//!
//! **Maintenance.** Call [`Controller::purge_expired_logins`] now and then.
//!
//! # Modules
//!
//! - [`store`]: the owner-scoped SQLite client store, with column-targeted writes;
//! - `hub`: one owner's live state (machines, session cache, state feed, session fan-out, UI
//!   clients) and its event bus;
//! - `machines`: the per-machine tasks (connect, bootstrap, reconnect with backoff,
//!   auto-upgrade) and the daemon link;
//! - `rpc`: the UI API (connection handling, ordering contract, handlers by namespace,
//!   table-driven pass-through);
//! - `attention` and `notify`: server-owned attention and Web Push delivery;
//! - `schedules`: the scheduler that wakes sessions on cron or interval specs ([`Clock`] in
//!   [`Tuning`] is its time source).

mod agentd;
mod attention;
mod auth;
mod config;
mod controller;
mod errors;
mod hub;
mod keys;
mod machines;
mod notify;
mod rotation;
mod router;
mod rpc;
mod schedules;
pub mod store;
#[cfg(all(unix, feature = "test-support"))]
pub mod test_support;

pub use auth::{
    AuthError, AuthPolicy, Authenticated, DEV_SUBJECT, Identity, NewLogin, Principal, token_hash,
};
pub use config::{Backoff, ControllerConfig, Tuning};
pub use controller::{Controller, StartError};
pub use controlroom_client_core::MasterKey;
pub use hub::CloseReason;
pub use notify::{PushConfig, PushOutcome, PushSender, VapidKey, VapidKeyError, WebPushSender};
pub use rpc::{Closed, UiClient, UiReceiver, UiSender};
pub use schedules::{Clock, ManualClock};
pub use store::OwnerId;
