//! The embedded UI (`web/dist`).
//!
//! The Nix package copies the built UI to `web/dist` before compiling, and it is embedded with
//! rust-embed; builds without it (plain `cargo build`, the test check) serve a placeholder.
//! Source maps are never embedded.
//!
//! - `/` and the files of the build are served; the app uses relative asset paths and hash
//!   routes (`#/s/<machine>/<session>`), so it lives at the root.
//! - SPA fallback: an unknown single-segment path without an extension gets `index.html`
//!   (deeper paths would break the relative asset URLs, so they are 404s, as are unknown files
//!   and anything under `api/` and `auth/`).
//! - Hashed build output under `assets/` is cached immutably; everything else (`index.html`,
//!   `sw.js`, the manifest, icons) is `no-cache`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

use crate::server::AppState;

/// `web/dist` without source maps.
#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/dist"]
#[exclude = "*.map"]
#[allow_missing = true]
struct Dist;

/// Served when no UI is embedded. No inline script or style.
const PLACEHOLDER_HTML: &str = "<!doctype html>\n<html lang=\"en\">\n<head>\n\
<meta charset=\"utf-8\">\n<title>controlroom</title>\n</head>\n<body>\n<h1>controlroom</h1>\n\
<p>This build doesn't include the web UI (web/dist was missing at build time).</p>\n\
</body>\n</html>\n";

/// Where the UI files come from.
#[derive(Clone)]
pub enum Assets {
    /// The build embedded at compile time.
    Embedded,
    /// Files in memory, by path relative to the root (tests, embedding).
    Memory(Arc<HashMap<String, Vec<u8>>>),
}

impl Assets {
    fn get(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        match self {
            Self::Embedded => Dist::get(path).map(|f| f.data),
            Self::Memory(files) => files.get(path).map(|v| Cow::Owned(v.clone())),
        }
    }
}

/// The `Content-Type` for a file name.
fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
    match ext.to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn cache_control(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

fn file_response(path: &str, bytes: Cow<'static, [u8]>, head: bool) -> Response {
    let body = if head {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    let mut res = Response::new(body);
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control(path)),
    );
    res
}

/// The router fallback: UI files and the SPA fallback.
pub(crate) async fn serve(
    State(state): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let head = method == Method::HEAD;
    let path = uri.path().trim_start_matches('/');
    let first = path.split('/').next().unwrap_or_default();
    if matches!(first, "api" | "auth") || path.split('/').any(|s| s == "." || s == "..") {
        return not_found();
    }
    if !path.is_empty() {
        if let Some(bytes) = state.assets.get(path) {
            return file_response(path, bytes, head);
        }
        if path.contains(['/', '.']) {
            return not_found();
        }
    }
    let index = state
        .assets
        .get("index.html")
        .unwrap_or(Cow::Borrowed(PLACEHOLDER_HTML.as_bytes()));
    file_response("index.html", index, head)
}

/// `/api/files/…` and `/api/stream/…`, reserved for the file transfer and terminal streams.
pub(crate) async fn reserved() -> Response {
    (StatusCode::NOT_IMPLEMENTED, "reserved\n").into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found\n").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_and_caching() {
        assert_eq!(
            content_type("manifest.webmanifest"),
            "application/manifest+json"
        );
        assert_eq!(content_type("sw.js"), "text/javascript; charset=utf-8");
        assert_eq!(
            content_type("assets/index-3f2a.CSS"),
            "text/css; charset=utf-8"
        );
        assert_eq!(content_type("noext"), "application/octet-stream");
        assert_eq!(
            cache_control("assets/index-3f2a.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(cache_control("index.html"), "no-cache");
        assert_eq!(cache_control("sw.js"), "no-cache");
    }
}
