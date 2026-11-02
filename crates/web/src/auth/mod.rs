//! Authentication: login sessions live in the controller's store; the shell verifies logins
//! (OIDC or the dev token), sets the cookie, and resolves it on every request through
//! [`controlroom_controller::Controller::authenticate`], which slides the expiry and re-checks
//! the auth mode and allow-list.
//!
//! - `GET /auth/session`: 200 `{csrf_token, auth, user}` (and a refreshed cookie), or 401
//!   `{login_url}` (relative to the app root; `null` in dev-token mode). The UI calls it
//!   before every WebSocket connect.
//! - `POST /auth/logout` (CSRF-checked): revokes the login and closes its WebSockets.
//! - OIDC: `GET /auth/login`, `GET /auth/callback` ([`oidc`]).
//! - Dev token: `GET /auth/dev-token?token=…` ([`dev_token`]).

pub mod cookies;
pub mod dev_token;
pub mod limiter;
pub mod login_state;
pub mod oidc;

use std::sync::Arc;

use axum::Json;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use controlroom_controller::{AuthError, Authenticated, Identity};
use serde_json::json;

use self::cookies::read_cookie;
use crate::server::AppState;

/// The configured way users sign in.
pub enum AuthRuntime {
    /// Dev-token mode.
    DevToken(dev_token::DevToken),
    /// OIDC.
    Oidc(Box<oidc::Oidc>),
}

impl AuthRuntime {
    /// Where the UI sends a signed-out user, relative to the app root.
    fn login_url(&self) -> Option<&'static str> {
        match self {
            Self::Oidc(_) => Some("auth/login"),
            Self::DevToken(_) => None,
        }
    }
}

/// Why a request isn't authenticated.
#[derive(Debug)]
pub(crate) enum Rejection {
    /// No cookie, or no live (admitted) login session for it.
    SignedOut,
    /// The store failed.
    Unavailable,
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        match self {
            Self::SignedOut => (StatusCode::UNAUTHORIZED, "sign in first\n").into_response(),
            Self::Unavailable => {
                (StatusCode::SERVICE_UNAVAILABLE, "try again later\n").into_response()
            }
        }
    }
}

/// Resolves the request's login cookie.
pub(crate) fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Authenticated, Rejection> {
    let token = read_cookie(headers, state.cookies.session_name()).ok_or(Rejection::SignedOut)?;
    match state.controller.authenticate(&token) {
        Ok(auth) => Ok(auth),
        Err(AuthError::Store(e)) => {
            tracing::error!(error = %e, "looking up a login session");
            Err(Rejection::Unavailable)
        }
        Err(e) => {
            tracing::debug!(error = %e, "login refused");
            Err(Rejection::SignedOut)
        }
    }
}

/// An authenticated request. The CSRF guard may already have resolved it.
pub(crate) struct Session(pub Authenticated);

impl FromRequestParts<Arc<AppState>> for Session {
    type Rejection = Rejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        if let Some(auth) = parts.extensions.get::<Authenticated>() {
            return Ok(Self(auth.clone()));
        }
        authenticate(state, &parts.headers).map(Self)
    }
}

/// Creates a login session for a verified identity and redirects to `return_to` with the
/// cookie set; 403 when the allow-list doesn't admit it.
pub(crate) fn login_response(state: &AppState, identity: &Identity, return_to: &str) -> Response {
    let principal = match state.controller.resolve(identity) {
        Ok(principal) => principal,
        Err(e) => {
            tracing::warn!(error = %e, "sign-in refused");
            return (
                StatusCode::FORBIDDEN,
                no_store(),
                "access denied for this account\n",
            )
                .into_response();
        }
    };
    match state.controller.create_login(&principal, None) {
        Ok(login) => {
            let cookie = state.cookies.set(
                state.cookies.session_name(),
                &login.token,
                state.session_ttl,
            );
            tracing::info!(subject = %principal.subject, "signed in");
            (
                StatusCode::SEE_OTHER,
                no_store(),
                [(header::LOCATION, return_to.to_owned())],
                [(header::SET_COOKIE, cookie)],
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "creating a login session");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn no_store() -> [(header::HeaderName, &'static str); 1] {
    [(header::CACHE_CONTROL, "no-store")]
}

/// `GET /auth/session`.
pub(crate) async fn session_info(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    match authenticate(&state, &headers) {
        Ok(auth) => {
            // Slide the cookie along with the stored session.
            let token = read_cookie(&headers, state.cookies.session_name()).unwrap_or_default();
            let cookie = state
                .cookies
                .set(state.cookies.session_name(), &token, state.session_ttl);
            let body = json!({
                "csrf_token": auth.csrf_token,
                "auth": state.controller.auth_mode(),
                "user": auth.principal.user_info(),
            });
            (no_store(), [(header::SET_COOKIE, cookie)], Json(body)).into_response()
        }
        Err(Rejection::SignedOut) => {
            let mut res = (
                StatusCode::UNAUTHORIZED,
                no_store(),
                Json(json!({ "login_url": state.auth.login_url() })),
            )
                .into_response();
            if read_cookie(&headers, state.cookies.session_name()).is_some() {
                res.headers_mut().append(
                    header::SET_COOKIE,
                    state.cookies.clear(state.cookies.session_name()),
                );
            }
            res
        }
        Err(rejection) => rejection.into_response(),
    }
}

/// `POST /auth/logout`: revokes the login session and closes its WebSockets.
pub(crate) async fn logout(State(state): State<Arc<AppState>>, Session(auth): Session) -> Response {
    if let Err(e) = state.controller.logout(&auth) {
        tracing::error!(error = %e, "revoking a login session");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    (
        StatusCode::NO_CONTENT,
        no_store(),
        [(
            header::SET_COOKIE,
            state.cookies.clear(state.cookies.session_name()),
        )],
    )
        .into_response()
}

/// Restricts post-login redirects to local absolute paths.
pub(crate) fn sanitize_return_to(value: Option<&str>) -> String {
    match value {
        Some(v)
            if v.starts_with('/')
                && !v.starts_with("//")
                && !v.contains('\\')
                && !v.chars().any(char::is_control) =>
        {
            v.to_owned()
        }
        _ => "/".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_to_is_local_only() {
        assert_eq!(sanitize_return_to(Some("/#/s/m/s")), "/#/s/m/s");
        assert_eq!(sanitize_return_to(Some("//evil.example")), "/");
        assert_eq!(sanitize_return_to(Some("https://evil.example")), "/");
        assert_eq!(sanitize_return_to(Some("/\\evil.example")), "/");
        assert_eq!(sanitize_return_to(Some("/a\nb")), "/");
        assert_eq!(sanitize_return_to(None), "/");
    }
}
