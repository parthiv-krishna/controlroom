//! controlroom-web: the HTTP shell around [`controlroom_controller`] (spec §12, §14). It
//! serves the embedded UI, the UI API over one WebSocket per tab, OIDC or dev-token sign-in,
//! and request hardening.
//!
//! - [`config`]: the TOML file, flags and environment, validated with the startup refusals
//!   (master key required, dev-token only on loopback, OIDC needs an https public URL);
//! - [`server`]: startup and the router;
//! - [`auth`]: login sessions (in the controller's store), the cookie, OIDC with PKCE, the
//!   dev token, and the login rate limit;
//! - [`hardening`], [`origin`], [`csp`]: the Host allowlist, CSRF, the security headers and
//!   the CSP;
//! - [`ws`]: the WebSocket bridge into a controller `UiClient`;
//! - [`terminal`]: terminal stream WebSockets into a controller `TerminalStream`;
//! - [`static_files`]: the embedded UI;
//! - [`secrets`]: credential files and tokens.
//!
//! The network placement: the server listens on loopback or a unix socket behind a reverse
//! proxy that terminates TLS. `public_url` is configured explicitly; it defines the OIDC
//! redirect URI, the Host allowlist and the expected Origin. `X-Forwarded-*` is never trusted.

pub mod auth;
pub mod config;
pub mod csp;
pub mod hardening;
pub mod origin;
pub mod secrets;
pub mod server;
pub mod static_files;
pub mod terminal;
pub mod ws;

pub use config::{Cli, Config, Env, Settings};
pub use server::{Server, StartOptions, VERSION, start};
pub use static_files::Assets;
