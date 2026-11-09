//! What the server's configuration (Nix, or files edited by hand) says about model providers:
//! `sandbox.toml`'s routes and pi's `models.json`, `auth.json` and `settings.json` in the
//! agent daemon's agent dir. Read-only here.
//!
//! A provider is **server-declared** when `sandbox.toml` has a route for it, `models.json`
//! defines it, or `auth.json` holds an API key for it. Settings › Models doesn't manage
//! server-declared providers: the server's configuration wins on a name clash. A sign-in
//! (an OAuth credential in `auth.json`, written by pi) is not configuration: it can be added
//! and removed from the UI for any provider pi can sign in to.

use controlroom_protocol::agentd::ModelDefaults;
use controlroom_sandbox::AbsPath;
use controlroom_sandbox::spec::CredentialRoute;
use serde_json::{Map, Value};

use crate::config::sandbox::SandboxSettings;

/// The server's provider configuration.
#[derive(Debug, Clone, Default)]
pub struct ServerConfig {
    /// `sandbox.toml`'s routes.
    pub routes: Vec<CredentialRoute>,
    /// The agent dir.
    pub agent_dir: Option<AbsPath>,
    /// `models.json`'s `providers`.
    pub models: Map<String, Value>,
    /// `auth.json`.
    pub auth: Map<String, Value>,
    /// The defaults in `settings.json`.
    pub defaults: ModelDefaults,
}

impl ServerConfig {
    /// Reads the files of `sandbox`'s agent dir. Unreadable files count as empty (the session
    /// launch reports them).
    pub fn load(sandbox: &SandboxSettings) -> Self {
        let agent_dir = sandbox.agent_dir.user_agent_dir.clone();
        let read = |name: &str| -> Map<String, Value> {
            let Some(dir) = &agent_dir else {
                return Map::new();
            };
            std::fs::read(dir.join(name).as_path())
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default()
        };
        let models = match read("models.json").remove("providers") {
            Some(Value::Object(providers)) => providers,
            _ => Map::new(),
        };
        let settings = read("settings.json");
        let text = |key: &str| settings.get(key).and_then(Value::as_str).map(str::to_owned);
        Self {
            routes: sandbox.routes.clone(),
            models,
            auth: read("auth.json"),
            defaults: ModelDefaults {
                provider: text("defaultProvider"),
                model: text("defaultModel"),
                thinking: text("defaultThinkingLevel"),
            },
            agent_dir,
        }
    }

    /// The server's route for `provider`.
    pub fn route(&self, provider: &str) -> Option<&CredentialRoute> {
        self.routes.iter().find(|r| r.provider.as_str() == provider)
    }

    /// Why `provider` is server-declared, if it is.
    pub fn declares(&self, provider: &str) -> Option<&'static str> {
        if self.route(provider).is_some() {
            Some("sandbox.toml has a route for it")
        } else if self.models.contains_key(provider) {
            Some("models.json defines it")
        } else if self.auth_type(provider) == Some("api_key") {
            Some("auth.json has a key for it")
        } else {
            None
        }
    }

    /// Every server-declared provider.
    pub fn declared(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .routes
            .iter()
            .map(|r| r.provider.as_str().to_owned())
            .chain(self.models.keys().cloned())
            .chain(
                self.auth
                    .keys()
                    .filter(|p| self.auth_type(p) == Some("api_key"))
                    .cloned(),
            )
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Whether `provider` has an API key in the server's config (`models.json` or
    /// `auth.json`).
    pub fn has_key(&self, provider: &str) -> bool {
        self.auth_type(provider) == Some("api_key")
            || self
                .models
                .get(provider)
                .and_then(|p| p.get("apiKey"))
                .is_some_and(Value::is_string)
    }

    /// The type of `provider`'s `auth.json` entry (`api_key`, `oauth`).
    pub fn auth_type(&self, provider: &str) -> Option<&str> {
        self.auth.get(provider)?.get("type")?.as_str()
    }

    /// Providers signed in (OAuth credentials in `auth.json`).
    pub fn signed_in(&self) -> Vec<String> {
        self.auth
            .keys()
            .filter(|p| self.auth_type(p) == Some("oauth"))
            .cloned()
            .collect()
    }

    /// The account of `provider`'s sign-in, when the credential names one: an `email` field,
    /// or an `email` claim of its access token (top level or one object deep).
    pub fn account(&self, provider: &str) -> Option<String> {
        let entry = self.auth.get(provider)?;
        if let Some(email) = entry.get("email").and_then(Value::as_str) {
            return Some(email.to_owned());
        }
        let claims = entry
            .get("access")
            .and_then(Value::as_str)
            .and_then(jwt_payload)?;
        let email = |v: &Value| v.get("email").and_then(Value::as_str).map(str::to_owned);
        email(&claims).or_else(|| claims.as_object()?.values().find_map(email))
    }
}

/// The payload of a JWT, unverified.
fn jwt_payload(token: &str) -> Option<Value> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use serde_json::json;

    use super::*;

    fn config(auth: Value, models: Value) -> ServerConfig {
        ServerConfig {
            auth: auth.as_object().unwrap().clone(),
            models: models.as_object().unwrap().clone(),
            ..ServerConfig::default()
        }
    }

    #[test]
    fn declared_providers_and_sign_ins() {
        let token = |claims: Value| {
            let p = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&claims).unwrap());
            format!("h.{p}.s")
        };
        let c = config(
            json!({
                "keyed": {"type": "api_key", "key": "k"},
                "sub": {"type": "oauth", "access": token(json!({"profile": {"email": "alice@example.com"}}))},
                "opaque": {"type": "oauth", "access": "sk-ant-oat01-x"}
            }),
            json!({"gw": {"baseUrl": "https://gw.example.com/v1", "apiKey": "!cat key"}}),
        );
        assert_eq!(c.declared(), ["gw", "keyed"]);
        assert!(c.declares("sub").is_none());
        assert!(c.has_key("gw") && c.has_key("keyed") && !c.has_key("sub"));
        assert_eq!(c.signed_in(), ["opaque", "sub"]);
        assert_eq!(c.account("sub").as_deref(), Some("alice@example.com"));
        assert_eq!(c.account("opaque"), None);
    }
}
