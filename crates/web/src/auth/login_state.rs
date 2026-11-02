//! The state of an OIDC login in flight (CSRF `state`, `nonce`, PKCE verifier, where to
//! return), sealed into the short-lived login cookie instead of a server-side table.
//!
//! The prototype kept pending logins in a bounded map, so an unauthenticated client could fill
//! it and block real logins for minutes. Sealed cookies cost the server nothing per login. The
//! key is random per process: a login started before a restart fails with "login expired",
//! and the user retries. Replaying a callback fails at the identity provider (codes are
//! single-use), and the cookie is cleared after the callback either way.

use std::time::Duration;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use controlroom_protocol::UnixMillis;
use data_encoding::BASE64URL_NOPAD;
use serde::{Deserialize, Serialize};

use crate::secrets::random_bytes;

/// How long a started login may take.
pub const LOGIN_TTL: Duration = Duration::from_secs(10 * 60);

const NONCE_LEN: usize = 12;
/// Bound on cookie values we try to open.
const MAX_SEALED_LEN: usize = 2048;

/// A login in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginState {
    /// The OAuth `state` (CSRF protection for the callback).
    pub state: String,
    /// The OIDC `nonce` (binds the ID token to this login).
    pub nonce: String,
    /// The PKCE code verifier.
    pub pkce_verifier: String,
    /// The local path to return to.
    pub return_to: String,
    /// When the login expires.
    pub expires_at: UnixMillis,
}

/// Seals and opens [`LoginState`]s with a per-process key.
pub struct LoginSealer {
    cipher: ChaCha20Poly1305,
}

impl Default for LoginSealer {
    /// A sealer with a fresh random key.
    fn default() -> Self {
        let key = random_bytes::<32>();
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(&key)),
        }
    }
}

impl LoginSealer {
    /// Seals `login` into a cookie value bound to `cookie_name`.
    pub fn seal(&self, cookie_name: &str, login: &LoginState) -> String {
        let plain = serde_json::to_vec(login).expect("a login state serializes");
        let nonce = random_bytes::<NONCE_LEN>();
        let sealed = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plain,
                    aad: cookie_name.as_bytes(),
                },
            )
            .expect("sealing a small message can't fail");
        let mut out = nonce.to_vec();
        out.extend_from_slice(&sealed);
        BASE64URL_NOPAD.encode(&out)
    }

    /// Opens a cookie value; `None` if it was forged, tampered with, sealed by another process
    /// or has expired.
    pub fn open(&self, cookie_name: &str, value: &str, now: UnixMillis) -> Option<LoginState> {
        if value.len() > MAX_SEALED_LEN {
            return None;
        }
        let bytes = BASE64URL_NOPAD.decode(value.as_bytes()).ok()?;
        if bytes.len() <= NONCE_LEN {
            return None;
        }
        let (nonce, sealed) = bytes.split_at(NONCE_LEN);
        let plain = self
            .cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: sealed,
                    aad: cookie_name.as_bytes(),
                },
            )
            .ok()?;
        let login: LoginState = serde_json::from_slice(&plain).ok()?;
        (now < login.expires_at).then_some(login)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login() -> LoginState {
        LoginState {
            state: "s".into(),
            nonce: "n".into(),
            pkce_verifier: "v".into(),
            return_to: "/".into(),
            expires_at: UnixMillis(2_000),
        }
    }

    #[test]
    fn round_trips_and_refuses_tampering_and_expiry() {
        let sealer = LoginSealer::default();
        let sealed = sealer.seal("login", &login());
        assert_eq!(
            sealer.open("login", &sealed, UnixMillis(1_000)),
            Some(login())
        );
        assert_eq!(sealer.open("login", &sealed, UnixMillis(2_000)), None);
        assert_eq!(sealer.open("other", &sealed, UnixMillis(1_000)), None);
        assert_eq!(
            LoginSealer::default().open("login", &sealed, UnixMillis(1_000)),
            None
        );
        let mut bytes = BASE64URL_NOPAD.decode(sealed.as_bytes()).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        let tampered = BASE64URL_NOPAD.encode(&bytes);
        assert_eq!(sealer.open("login", &tampered, UnixMillis(1_000)), None);
        assert_eq!(sealer.open("login", "garbage!", UnixMillis(1_000)), None);
        assert_eq!(sealer.open("login", "", UnixMillis(1_000)), None);
    }
}
