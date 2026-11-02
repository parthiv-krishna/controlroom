//! Web Push delivery: RFC 8291 encryption and RFC 8292 VAPID authentication (pure RustCrypto,
//! via `web-push-native`), sent with reqwest over rustls.

use std::path::Path;
use std::time::Duration;

use data_encoding::BASE64URL_NOPAD;
use futures::future::BoxFuture;
use web_push_native::jwt_simple::algorithms::{ECDSAP256PublicKeyLike, ES256KeyPair};
use web_push_native::{Auth, WebPushBuilder, p256};

use super::{PushOutcome, PushSender};
use crate::store::PushSubscriptionRow;

/// How long push services keep an undelivered message.
const PUSH_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// Why the VAPID key couldn't be loaded.
#[derive(Debug, thiserror::Error)]
#[error("VAPID key {path}: {detail}")]
pub struct VapidKeyError {
    path: String,
    detail: String,
}

/// The server's VAPID identity.
pub struct VapidKey {
    key_pair: ES256KeyPair,
}

impl VapidKey {
    /// Loads the private key from a credential file: a base64url raw P-256 scalar, or a PEM
    /// private key.
    pub fn load(path: &Path) -> Result<Self, VapidKeyError> {
        let error = |detail: String| VapidKeyError {
            path: path.display().to_string(),
            detail,
        };
        let text = std::fs::read_to_string(path).map_err(|e| error(e.to_string()))?;
        Self::parse(&text).map_err(error)
    }

    /// Parses a base64url raw scalar or a PEM private key.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let key_pair = if text.starts_with("-----BEGIN") {
            ES256KeyPair::from_pem(text).map_err(|e| e.to_string())?
        } else {
            let raw = BASE64URL_NOPAD
                .decode(text.trim_end_matches('=').as_bytes())
                .map_err(|e| e.to_string())?;
            ES256KeyPair::from_bytes(&raw).map_err(|e| e.to_string())?
        };
        Ok(Self { key_pair })
    }

    /// A fresh key (for tests and first-time setup tooling).
    pub fn generate() -> Self {
        Self {
            key_pair: ES256KeyPair::generate(),
        }
    }

    /// The base64url uncompressed public key: the browser's `applicationServerKey`.
    pub fn public_key_base64url(&self) -> String {
        BASE64URL_NOPAD.encode(
            &self
                .key_pair
                .public_key()
                .public_key()
                .to_bytes_uncompressed(),
        )
    }
}

/// Sends Web Push messages to push services.
pub struct WebPushSender {
    vapid: VapidKey,
    /// The VAPID `sub` claim: a `mailto:` or `https:` contact.
    subject: String,
    http: reqwest::Client,
}

impl WebPushSender {
    /// A sender authenticating as `vapid`, with contact `subject`.
    pub fn new(vapid: VapidKey, subject: String) -> Self {
        Self {
            vapid,
            subject,
            http: reqwest::Client::new(),
        }
    }

    fn request(
        &self,
        sub: &PushSubscriptionRow,
        payload: &[u8],
    ) -> Result<reqwest::Request, String> {
        let endpoint = sub.endpoint.parse().map_err(|e| format!("endpoint: {e}"))?;
        let decode = |s: &str| {
            BASE64URL_NOPAD
                .decode(s.trim_end_matches('=').as_bytes())
                .map_err(|e| e.to_string())
        };
        let p256dh = decode(&sub.p256dh)?;
        let auth = decode(&sub.auth)?;
        if auth.len() != 16 {
            return Err("the push auth secret must be 16 bytes".into());
        }
        let request = WebPushBuilder::new(
            endpoint,
            p256::PublicKey::from_sec1_bytes(&p256dh).map_err(|e| e.to_string())?,
            Auth::clone_from_slice(&auth),
        )
        .with_valid_duration(PUSH_TTL)
        .with_vapid(&self.vapid.key_pair, &self.subject)
        .build(payload.to_vec())
        .map_err(|e| e.to_string())?;
        reqwest::Request::try_from(request).map_err(|e| e.to_string())
    }
}

impl PushSender for WebPushSender {
    fn send<'a>(
        &'a self,
        sub: &'a PushSubscriptionRow,
        payload: &'a [u8],
        urgent: bool,
    ) -> BoxFuture<'a, PushOutcome> {
        Box::pin(async move {
            let mut request = match self.request(sub, payload) {
                Ok(request) => request,
                Err(e) => return PushOutcome::Failed(format!("building the push: {e}")),
            };
            let urgency = if urgent { "high" } else { "normal" };
            request.headers_mut().insert(
                "urgency",
                reqwest::header::HeaderValue::from_static(urgency),
            );
            *request.timeout_mut() = Some(SEND_TIMEOUT);
            match self.http.execute(request).await {
                Ok(res) if res.status().is_success() => PushOutcome::Delivered,
                Ok(res) if matches!(res.status().as_u16(), 404 | 410) => PushOutcome::Gone,
                Ok(res) => PushOutcome::Failed(format!("push service answered {}", res.status())),
                Err(e) => PushOutcome::Failed(e.to_string()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::ids::DeviceId;

    use super::*;

    #[test]
    fn keys_round_trip_and_requests_are_encrypted() {
        let key = VapidKey::generate();
        let raw = BASE64URL_NOPAD.encode(&key.key_pair.to_bytes());
        let parsed = VapidKey::parse(&raw).unwrap();
        assert_eq!(parsed.public_key_base64url(), key.public_key_base64url());
        assert!(VapidKey::parse("not a key").is_err());

        let browser = ES256KeyPair::generate();
        let sub = PushSubscriptionRow {
            device_id: DeviceId::new("phone"),
            endpoint: "https://push.example.com/send/1".into(),
            p256dh: BASE64URL_NOPAD
                .encode(&browser.public_key().public_key().to_bytes_uncompressed()),
            auth: BASE64URL_NOPAD.encode(&[7u8; 16]),
        };
        let sender = WebPushSender::new(key, "mailto:alice@example.com".into());
        let request = sender.request(&sub, b"{\"state\":\"approval\"}").unwrap();
        assert_eq!(request.url().as_str(), "https://push.example.com/send/1");
        assert!(request.headers().contains_key("authorization"));
        let body = request.body().and_then(|b| b.as_bytes()).unwrap();
        assert!(!body.windows(8).any(|w| w == b"approval"), "encrypted");
        let mut bad = sub.clone();
        bad.auth = BASE64URL_NOPAD.encode(&[1u8; 3]);
        assert!(sender.request(&bad, b"x").is_err());
    }
}
