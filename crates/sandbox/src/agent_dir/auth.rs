//! The session `auth.json`: fake tokens only (spec §11.3).
//!
//! - API-key providers get [`PLACEHOLDER`]; nono's proxy injects the real key, fetched
//!   host-side by `controlroomd credential`.
//! - OAuth subscriptions get a synthetic unsigned JWT carrying only the non-secret claims pi
//!   needs (e.g. an account id), a placeholder refresh token and a far-future expiry, so the
//!   sandbox never attempts a refresh (an attempt would fail closed at the proxy). Opaque tokens
//!   that pi tells apart by a marker keep it (see [`OPAQUE_MARKERS`]).

use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value, json};

use super::PLACEHOLDER;

/// `expires` of synthetic OAuth entries: 2100-01-01T00:00:00Z in unix ms.
pub const FAR_FUTURE_MS: i64 = 4_102_444_800_000;

/// Markers of opaque OAuth access tokens that pi reads: pi's Anthropic API sends a token as a
/// subscription (`Authorization: Bearer` and Claude Code headers) only when it contains
/// `sk-ant-oat`, and as an API key (`x-api-key`) otherwise. The synthetic token keeps the
/// real one's type prefix (`sk-ant-oat01-`), which is no secret.
const OPAQUE_MARKERS: [&str; 1] = ["sk-ant-oat"];

/// The sandbox auth document plus what the credential helper needs to know.
#[derive(Debug, Clone, PartialEq)]
pub struct FakeAuth {
    /// The `auth.json` written into the session agent dir.
    pub auth: Map<String, Value>,
    /// Providers whose real credential is an API key.
    pub api_key_providers: Vec<String>,
    /// Providers whose real credential is an OAuth token.
    pub oauth_providers: Vec<String>,
}

/// Builds the fake auth document from the user's real one.
///
/// `oauth_claims` lists, per provider, the top-level JWT claims copied from the real access
/// token into the synthetic one. Entries of unknown `type` are dropped.
pub fn fake_auth(
    real: &Map<String, Value>,
    oauth_claims: &BTreeMap<String, Vec<String>>,
) -> FakeAuth {
    let mut out = FakeAuth {
        auth: Map::new(),
        api_key_providers: Vec::new(),
        oauth_providers: Vec::new(),
    };
    for (provider, entry) in real {
        match entry.get("type").and_then(Value::as_str) {
            Some("api_key") => {
                let mut fake = Map::new();
                fake.insert("type".into(), "api_key".into());
                fake.insert("key".into(), PLACEHOLDER.into());
                if let Some(env) = entry.get("env").and_then(Value::as_object) {
                    fake.insert("env".into(), Value::Object(literal_env(env)));
                }
                out.auth.insert(provider.clone(), Value::Object(fake));
                out.api_key_providers.push(provider.clone());
            }
            Some("oauth") => {
                let claims = oauth_claims
                    .get(provider)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                out.auth
                    .insert(provider.clone(), Value::Object(fake_oauth(entry, claims)));
                out.oauth_providers.push(provider.clone());
            }
            _ => {}
        }
    }
    out
}

/// Keeps literal provider env values (e.g. an account id); drops `!command` values, which
/// may resolve to secrets.
fn literal_env(env: &Map<String, Value>) -> Map<String, Value> {
    env.iter()
        .filter(|(_, v)| v.as_str().is_some_and(|s| !s.starts_with('!')))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Field names of OAuth entries that hold token material.
fn is_secret_field(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["access", "refresh", "token", "secret", "key", "password"]
        .iter()
        .any(|w| n.contains(w))
}

fn fake_oauth(entry: &Value, claims: &[String]) -> Map<String, Value> {
    let real_claims = entry
        .get("access")
        .and_then(Value::as_str)
        .and_then(jwt_payload);
    let mut payload = Map::new();
    if let Some(real) = &real_claims {
        for claim in claims {
            if let Some(v) = real.get(claim) {
                payload.insert(claim.clone(), v.clone());
            }
        }
    }
    payload.insert("exp".into(), json!(FAR_FUTURE_MS / 1000));

    let mut fake = Map::new();
    // Non-token metadata (e.g. an enterprise domain) is kept; token fields are not.
    if let Some(obj) = entry.as_object() {
        for (k, v) in obj {
            if !is_secret_field(k) && k != "expires" {
                fake.insert(k.clone(), v.clone());
            }
        }
    }
    let access = entry.get("access").and_then(Value::as_str);
    let synthetic = access
        .and_then(marked_placeholder)
        .unwrap_or_else(|| unsigned_jwt(&payload));
    fake.insert("type".into(), "oauth".into());
    fake.insert("access".into(), synthetic.into());
    fake.insert("refresh".into(), PLACEHOLDER.into());
    fake.insert("expires".into(), json!(FAR_FUTURE_MS));
    fake
}

/// What stands in for `credential` inside the sandbox: [`PLACEHOLDER`], behind the type prefix
/// of a token pi tells apart by a marker (e.g. a pasted Anthropic subscription token), so pi
/// takes the same code path as with the real one. The prefix is no secret.
pub fn stand_in(credential: &str) -> String {
    marked_placeholder(credential).unwrap_or_else(|| PLACEHOLDER.to_owned())
}

/// For an opaque token with one of [`OPAQUE_MARKERS`], the placeholder behind its type prefix
/// (the dash-separated segments up to the one holding the marker).
fn marked_placeholder(token: &str) -> Option<String> {
    let marker = OPAQUE_MARKERS.iter().find(|m| token.starts_with(*m))?;
    let segments = marker.matches('-').count() + 1;
    // The prefix and at least one more segment (the token proper).
    let mut parts: Vec<&str> = token.splitn(segments + 1, '-').collect();
    if parts.len() <= segments {
        return None;
    }
    parts.truncate(segments);
    Some(format!("{}-{PLACEHOLDER}", parts.join("-")))
}

/// Decodes the payload of a JWT without verifying it.
fn jwt_payload(token: &str) -> Option<Map<String, Value>> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    match serde_json::from_slice(&bytes).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// An `alg: none` JWT with an empty signature.
pub fn unsigned_jwt(payload: &Map<String, Value>) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let body = URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(payload).expect("JSON map serialization cannot fail"));
    format!("{header}.{body}.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real_jwt(payload: Value) -> String {
        let p = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        format!("eyJhbGciOiJSUzI1NiJ9.{p}.c2lnbmF0dXJl")
    }

    #[test]
    fn api_keys_become_placeholders() {
        let real = json!({
            "gateway": {"type": "api_key", "key": "sk-real-secret",
                        "env": {"ACCOUNT_ID": "acct-1", "OTHER": "!pass show x"}},
            "weird": {"type": "magic", "key": "x"}
        });
        let out = fake_auth(real.as_object().unwrap(), &BTreeMap::new());
        assert_eq!(
            Value::Object(out.auth.clone()),
            json!({"gateway": {"type": "api_key", "key": PLACEHOLDER, "env": {"ACCOUNT_ID": "acct-1"}}})
        );
        assert_eq!(out.api_key_providers, ["gateway"]);
        assert!(
            !serde_json::to_string(&out.auth)
                .unwrap()
                .contains("sk-real")
        );
    }

    #[test]
    fn oauth_gets_a_synthetic_jwt_with_only_kept_claims() {
        let access = real_jwt(json!({
            "https://example.com/auth": {"account_id": "acct-42"},
            "email": "alice@example.com",
            "exp": 1
        }));
        let real = json!({
            "sub": {"type": "oauth", "access": access, "refresh": "rt-real-secret",
                    "expires": 5, "enterpriseUrl": "git.example.com", "idToken": "id-secret"}
        });
        let claims = BTreeMap::from([(
            "sub".to_owned(),
            vec!["https://example.com/auth".to_owned()],
        )]);
        let out = fake_auth(real.as_object().unwrap(), &claims);
        let entry = out.auth["sub"].as_object().unwrap();
        assert_eq!(entry["refresh"], PLACEHOLDER);
        assert_eq!(entry["expires"], json!(FAR_FUTURE_MS));
        assert_eq!(entry["enterpriseUrl"], "git.example.com");
        assert!(!entry.contains_key("idToken"));
        let fake = entry["access"].as_str().unwrap();
        assert!(fake.ends_with('.'), "unsigned: empty signature");
        let payload = jwt_payload(fake).unwrap();
        assert_eq!(
            Value::Object(payload),
            json!({"https://example.com/auth": {"account_id": "acct-42"}, "exp": FAR_FUTURE_MS / 1000})
        );
        let text = serde_json::to_string(&out.auth).unwrap();
        assert!(
            !text.contains("rt-real") && !text.contains("alice@") && !text.contains("id-secret")
        );
        assert_eq!(out.oauth_providers, ["sub"]);
    }

    #[test]
    fn marked_opaque_tokens_keep_their_type_prefix() {
        let real = json!({"anthropic": {"type": "oauth", "access": "sk-ant-oat01-real-secret",
                                       "refresh": "sk-ant-ort01-also-secret", "expires": 1}});
        let out = fake_auth(real.as_object().unwrap(), &BTreeMap::new());
        let access = out.auth["anthropic"]["access"].as_str().unwrap();
        assert_eq!(access, format!("sk-ant-oat01-{PLACEHOLDER}"));
        assert!(!serde_json::to_string(&out.auth).unwrap().contains("secret"));
        assert_eq!(marked_placeholder("sk-ant-oat"), None);
        assert_eq!(marked_placeholder("opaque"), None);
        assert_eq!(
            stand_in("sk-ant-oat01-test-secret"),
            format!("sk-ant-oat01-{PLACEHOLDER}")
        );
        assert_eq!(stand_in("sk-ant-api03-test-secret"), PLACEHOLDER);
    }

    #[test]
    fn opaque_oauth_tokens_get_a_claimless_jwt() {
        let real =
            json!({"p": {"type": "oauth", "access": "opaque", "refresh": "r", "expires": 1}});
        let out = fake_auth(
            real.as_object().unwrap(),
            &BTreeMap::from([("p".into(), vec!["sub".into()])]),
        );
        let payload = jwt_payload(out.auth["p"]["access"].as_str().unwrap()).unwrap();
        assert_eq!(Value::Object(payload), json!({"exp": FAR_FUTURE_MS / 1000}));
    }
}
