//! The login cookies: names, attributes and parsing.
//!
//! Over https the names carry the `__Host-` prefix (Secure, `Path=/`, no `Domain`), so a
//! sibling subdomain can't set or shadow them. Plain http only happens in dev-token mode on
//! loopback, where browsers would drop Secure cookies.

use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue, header};

/// Cookie names and attributes.
#[derive(Debug, Clone)]
pub struct Cookies {
    secure: bool,
}

impl Cookies {
    /// Cookies for an https (`secure`) or loopback http origin.
    pub fn new(secure: bool) -> Self {
        Self { secure }
    }

    /// The login session cookie.
    pub fn session_name(&self) -> &'static str {
        if self.secure {
            "__Host-controlroom_session"
        } else {
            "controlroom_session"
        }
    }

    /// The short-lived cookie carrying an OIDC login in flight.
    pub fn login_name(&self) -> &'static str {
        if self.secure {
            "__Host-controlroom_login"
        } else {
            "controlroom_login"
        }
    }

    /// A `Set-Cookie` value. `SameSite=Lax` so the identity provider's top-level redirect back
    /// to the callback carries the login cookie.
    pub fn set(&self, name: &str, value: &str, max_age: Duration) -> HeaderValue {
        let secure = if self.secure { "; Secure" } else { "" };
        HeaderValue::from_str(&format!(
            "{name}={value}; Path=/; Max-Age={}; HttpOnly; SameSite=Lax{secure}",
            max_age.as_secs()
        ))
        .expect("cookies are built from URL-safe token characters")
    }

    /// A `Set-Cookie` value that deletes the cookie.
    pub fn clear(&self, name: &str) -> HeaderValue {
        self.set(name, "", Duration::ZERO)
    }
}

/// The value of cookie `name` in the request headers.
pub fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_and_parsing() {
        let secure = Cookies::new(true);
        assert_eq!(
            secure.set(secure.session_name(), "abc", Duration::from_secs(60)),
            "__Host-controlroom_session=abc; Path=/; Max-Age=60; HttpOnly; SameSite=Lax; Secure"
        );
        let plain = Cookies::new(false);
        assert_eq!(
            plain.clear(plain.session_name()),
            "controlroom_session=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax"
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; __Host-controlroom_session=xyz"),
        );
        assert_eq!(
            read_cookie(&headers, "__Host-controlroom_session").as_deref(),
            Some("xyz")
        );
        assert_eq!(read_cookie(&headers, "b"), None);
    }
}
