//! Dev-token mode (loopback only, enforced by the configuration): a random token per run,
//! written `0600` to `<state_dir>/dev-token` and printed at start, Jupyter-style. Opening
//! `/auth/dev-token?token=…` exchanges it for a normal login session cookie.

use std::path::Path;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use controlroom_controller::Identity;
use serde::Deserialize;

use super::{AuthRuntime, login_response};
use crate::secrets::{constant_time_eq, random_token, write_private_file};
use crate::server::AppState;

/// The file the token is written to, in the state directory.
pub const TOKEN_FILE: &str = "dev-token";

/// This run's dev token.
pub struct DevToken {
    token: String,
}

impl DevToken {
    /// Generates a token and stores it in `state_dir`.
    pub fn generate(state_dir: &Path) -> std::io::Result<Self> {
        let token = random_token();
        write_private_file(&state_dir.join(TOKEN_FILE), format!("{token}\n").as_bytes())?;
        Ok(Self { token })
    }

    /// The token.
    pub fn token(&self) -> &str {
        &self.token
    }

    fn matches(&self, candidate: &str) -> bool {
        constant_time_eq(candidate, &self.token)
    }
}

#[derive(Deserialize)]
pub(crate) struct ExchangeQuery {
    token: Option<String>,
}

/// `GET /auth/dev-token?token=…`: a login session cookie, then the app.
pub(crate) async fn exchange(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ExchangeQuery>,
) -> Response {
    let AuthRuntime::DevToken(dev) = &state.auth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !query.token.as_deref().is_some_and(|t| dev.matches(t)) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CACHE_CONTROL, "no-store")],
            "invalid dev token\n",
        )
            .into_response();
    }
    login_response(&state, &Identity::DevToken, "/")
}
