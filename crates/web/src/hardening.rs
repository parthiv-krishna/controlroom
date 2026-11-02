//! Request hardening middleware: the Host allowlist, CSRF on state-changing requests, and the
//! security headers on every response.
//!
//! WebSocket upgrades are GETs; the WebSocket handler checks their `Origin` strictly instead
//! ([`crate::ws`]).

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::auth::authenticate;
use crate::secrets::constant_time_eq;
use crate::server::AppState;

/// The header carrying the CSRF token.
pub const CSRF_HEADER: &str = "x-csrf-token";

/// Paths exempt from the Host allowlist (local health probes use any address).
const HEALTH_PATH: &str = "/healthz";

/// Refuses requests whose `Host` isn't derived from the public URL (DNS rebinding).
pub(crate) async fn host_allowlist(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    if req.uri().path() == HEALTH_PATH {
        return next.run(req).await;
    }
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned)
        .or_else(|| req.uri().authority().map(|a| a.to_string()));
    match host {
        Some(host) if state.origin.host_allowed(&host) => next.run(req).await,
        _ => (StatusCode::MISDIRECTED_REQUEST, "unknown Host\n").into_response(),
    }
}

/// CSRF protection for every request but GET and HEAD: an allowed `Origin` when the browser
/// sends one, a login session, and its token in `X-CSRF-Token`. The resolved session is
/// handed on to the handler.
pub(crate) async fn csrf_guard(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD) {
        return next.run(req).await;
    }
    let headers = req.headers();
    if let Some(origin) = headers.get(header::ORIGIN) {
        let allowed = origin
            .to_str()
            .is_ok_and(|o| state.origin.origin_allowed(o));
        if !allowed {
            return (StatusCode::FORBIDDEN, "cross-origin request refused\n").into_response();
        }
    }
    let auth = match authenticate(&state, headers) {
        Ok(auth) => auth,
        Err(rejection) => return rejection.into_response(),
    };
    let token = headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok());
    if !token.is_some_and(|t| constant_time_eq(t, &auth.csrf_token)) {
        return (StatusCode::FORBIDDEN, "missing or invalid CSRF token\n").into_response();
    }
    req.extensions_mut().insert(auth);
    next.run(req).await
}

/// Adds the security headers. A handler's own CSP (e.g. `sandbox` on the reserved file
/// responses) is kept.
pub(crate) async fn security_headers(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    if !headers.contains_key(header::CONTENT_SECURITY_POLICY) {
        headers.insert(header::CONTENT_SECURITY_POLICY, state.csp.clone());
    }
    set(headers, "x-content-type-options", "nosniff");
    set(headers, "x-frame-options", "DENY");
    set(headers, "referrer-policy", "no-referrer");
    set(headers, "cross-origin-opener-policy", "same-origin");
    set(headers, "cross-origin-resource-policy", "same-origin");
    set(
        headers,
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
    );
    if state.origin.secure {
        set(headers, "strict-transport-security", "max-age=63072000");
    }
    res
}

fn set(headers: &mut HeaderMap, name: &'static str, value: &'static str) {
    if !headers.contains_key(name) {
        headers.insert(name, HeaderValue::from_static(value));
    }
}
