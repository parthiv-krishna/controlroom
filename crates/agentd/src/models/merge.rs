//! What Settings › Models adds to a launch: routes, synthetic-token claims, and the session's
//! stored keys (with the stand-ins of pasted subscription tokens), gateways and defaults
//! ([`ManagedProviders`]). Server-declared providers are
//! left alone (the server's configuration wins on a name clash), so the server's routes,
//! `models.json` entries and keys are never replaced.

use std::collections::BTreeMap;

use controlroom_protocol::agentd::{Gateway, KeyKind};
use controlroom_sandbox::AbsPath;
use controlroom_sandbox::agent_dir::{ManagedProviders, PLACEHOLDER, stand_in};
use controlroom_sandbox::credential::KeyStore;
use serde_json::{Map, Value, json};

use super::presets::{self, PRESETS};
use super::server::ServerConfig;
use super::store::{Store, Stored};
use crate::config::sandbox::{DEFAULT_CACHE_TTL_SECS, RouteConfig, SandboxSettings};
use crate::dirs::Dirs;

/// What Settings › Models adds to the server's configuration.
#[derive(Debug, Clone, Default)]
pub struct Additions {
    /// Routes of providers managed here.
    pub routes: Vec<RouteConfig>,
    /// Claims of subscriptions signed in here.
    pub claims: BTreeMap<String, Vec<String>>,
    /// Keys, gateways and defaults for the session's agent dir.
    pub managed: ManagedProviders,
}

/// Computes the additions; `key_store` is the stored keys' file.
pub fn additions(
    stored: &Stored,
    keys: &KeyStore,
    key_store: &AbsPath,
    server: &ServerConfig,
) -> Additions {
    let mut out = Additions {
        managed: ManagedProviders {
            key_store: Some(key_store.clone()),
            ..ManagedProviders::default()
        },
        ..Additions::default()
    };
    for preset in PRESETS {
        if server.declares(preset.id).is_some() {
            continue;
        }
        let signed_in = server.auth_type(preset.id) == Some("oauth");
        let token = keys
            .get(preset.id)
            .filter(|key| presets::key_kind(preset.id, key.expose()) == KeyKind::SubscriptionToken);
        match (preset.subscription, preset.api_key, token) {
            (Some(sub), _, _) if signed_in => {
                out.routes.push(sub.route.route(preset.id));
                if !sub.claims.is_empty() {
                    let claims = sub.claims.iter().map(|c| (*c).to_owned()).collect();
                    out.claims.insert(preset.id.to_owned(), claims);
                }
            }
            // A pasted subscription token: the sign-in's route, and a stand-in with the
            // token's type prefix, by which pi sends it as a subscription's.
            (Some(sub), _, Some(token)) => {
                out.routes.push(sub.route.route(preset.id));
                out.managed.key_providers.push(preset.id.to_owned());
                out.managed
                    .stand_ins
                    .insert(preset.id.to_owned(), stand_in(token.expose()));
            }
            (_, Some((route, _)), _) if keys.contains(preset.id) => {
                out.routes.push(route.route(preset.id));
                out.managed.key_providers.push(preset.id.to_owned());
            }
            _ => {}
        }
    }
    for (id, provider) in &stored.providers {
        let Some(gateway) = &provider.gateway else {
            continue;
        };
        if server.declares(id).is_some() || !keys.contains(id) {
            continue;
        }
        let Ok(route) = gateway_route(id, gateway) else {
            continue;
        };
        out.routes.push(route);
        out.managed.key_providers.push(id.clone());
        out.managed
            .models
            .insert(id.clone(), gateway_models_entry(gateway));
    }
    let defaults = &stored.defaults;
    for (key, value) in [
        ("defaultProvider", &defaults.provider),
        ("defaultModel", &defaults.model),
        ("defaultThinkingLevel", &defaults.thinking),
    ] {
        if let Some(value) = value {
            out.managed
                .settings
                .insert(key.to_owned(), Value::String(value.clone()));
        }
    }
    out
}

/// Adds Settings › Models to `sandbox` (resolved `sandbox.toml`) for a launch. A store that
/// can't be read, or a route that is no longer valid, is skipped with a warning: the server's
/// own providers keep working.
pub fn apply(dirs: &Dirs, sandbox: &mut SandboxSettings) {
    let store = Store::new(dirs.models());
    let (stored, keys) = match (store.load(), store.keys()) {
        (Ok(stored), Ok(keys)) => (stored, keys),
        (Err(e), _) | (_, Err(e)) => {
            tracing::warn!("Settings › Models ignored: {e}");
            return;
        }
    };
    let server = ServerConfig::load(sandbox);
    let additions = additions(&stored, &keys, &store.keys_path(), &server);
    for route in additions.routes {
        match route.resolve(sandbox.allow_loopback_upstreams) {
            Ok(route) => sandbox.routes.push(route),
            Err(e) => tracing::warn!("Settings › Models: {e}"),
        }
    }
    for (provider, claims) in additions.claims {
        sandbox
            .agent_dir
            .oauth_claims
            .entry(provider)
            .or_insert(claims);
    }
    sandbox.agent_dir.managed = additions.managed;
}

/// A gateway's base URL split into its origin and path (without a trailing `/`).
pub fn split_base_url(url: &str) -> Result<(String, String), String> {
    let invalid = || format!("{url:?} is not an http(s) URL without query or fragment");
    let (scheme, rest) = url.split_once("://").ok_or_else(invalid)?;
    if !matches!(scheme, "https" | "http") || url.contains(['?', '#', '*', ' ']) {
        return Err(invalid());
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return Err(invalid());
    }
    Ok((
        format!("{scheme}://{authority}"),
        path.trim_end_matches('/').to_owned(),
    ))
}

/// A gateway's route: chat completions under its base path.
pub fn gateway_route(provider: &str, gateway: &Gateway) -> Result<RouteConfig, String> {
    let (origin, path) = split_base_url(&gateway.base_url)?;
    let (header, format) =
        presets::gateway_header(gateway.auth_header).ok_or("unknown auth header style")?;
    Ok(RouteConfig {
        provider: provider.to_owned(),
        upstream: origin,
        allow: vec![format!("POST {path}/chat/completions")],
        approve: Vec::new(),
        inject_header: Some(header.to_owned()),
        credential_format: Some(format.to_owned()),
        cache_ttl_secs: DEFAULT_CACHE_TTL_SECS,
    })
}

/// A gateway's `models.json` provider entry, with placeholders for the key.
pub fn gateway_models_entry(gateway: &Gateway) -> Value {
    let models: Vec<Value> = gateway.models.iter().map(|id| json!({"id": id})).collect();
    let mut entry = json!({
        "baseUrl": gateway.base_url,
        "api": "openai-completions",
        "apiKey": PLACEHOLDER,
        "models": models,
    });
    // pi's OpenAI SDK always sends `Authorization`; another header gets a placeholder too, so
    // nono finds it to replace.
    if let Some((header, _)) = presets::gateway_header(gateway.auth_header)
        && header != "Authorization"
    {
        let mut headers = Map::new();
        headers.insert(header.to_owned(), Value::String(PLACEHOLDER.to_owned()));
        entry["headers"] = Value::Object(headers);
    }
    entry
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::UnixMillis;
    use controlroom_protocol::agentd::{AuthHeader, ModelDefaults, StoredKey};
    use serde_json::json;

    use super::super::store::StoredProvider;
    use super::*;

    fn key() -> Option<StoredKey> {
        Some(StoredKey {
            kind: KeyKind::ApiKey,
            last4: "abcd".into(),
            added_at: UnixMillis(1),
            added_by: None,
        })
    }

    fn gateway() -> Gateway {
        Gateway {
            base_url: "https://gw.example.com/v1/".into(),
            models: vec!["m1".into(), "m2".into()],
            auth_header: AuthHeader::XApiKey,
        }
    }

    #[test]
    fn presets_gateways_and_defaults() {
        let mut stored = Stored::default();
        for id in ["anthropic", "openai", "google"] {
            stored.providers.insert(
                id.into(),
                StoredProvider {
                    key: key(),
                    ..Default::default()
                },
            );
        }
        stored.providers.insert(
            "gw".into(),
            StoredProvider {
                key: key(),
                gateway: Some(gateway()),
                test: None,
            },
        );
        stored.defaults = ModelDefaults {
            provider: Some("gw".into()),
            model: Some("m1".into()),
            thinking: None,
        };
        let mut keys = KeyStore::default();
        for id in ["anthropic", "openai", "google", "gw"] {
            keys.insert(id, "secret");
        }
        // The server declares `openai` (a key of its own) and signed in to Anthropic and Codex.
        let server = ServerConfig {
            models: json!({"openai": {"apiKey": "!cat key"}})
                .as_object()
                .unwrap()
                .clone(),
            auth: json!({
                "anthropic": {"type": "oauth", "access": "sk-ant-oat01-x"},
                "openai-codex": {"type": "oauth", "access": "a.b.c"}
            })
            .as_object()
            .unwrap()
            .clone(),
            ..ServerConfig::default()
        };
        let store = AbsPath::new("/var/lib/controlroom-agent/models/keys.json").unwrap();
        let a = additions(&stored, &keys, &store, &server);
        let routes: Vec<_> = a
            .routes
            .iter()
            .map(|r| {
                (
                    r.provider.as_str(),
                    r.upstream.as_str(),
                    r.inject_header.as_deref().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            routes,
            [
                // Signed in: the subscription's route wins over the stored key.
                ("anthropic", "https://api.anthropic.com", "Authorization"),
                ("openai-codex", "https://chatgpt.com", "Authorization"),
                (
                    "google",
                    "https://generativelanguage.googleapis.com",
                    "x-goog-api-key"
                ),
                ("gw", "https://gw.example.com", "x-api-key"),
            ]
        );
        assert_eq!(a.routes[3].allow, ["POST /v1/chat/completions"]);
        assert_eq!(
            a.claims["openai-codex"],
            ["https://api.openai.com/auth".to_owned()]
        );
        assert_eq!(a.managed.key_providers, ["google", "gw"]);
        assert_eq!(a.managed.key_store, Some(store));
        assert_eq!(
            a.managed.models["gw"],
            json!({
                "baseUrl": "https://gw.example.com/v1/",
                "api": "openai-completions",
                "apiKey": PLACEHOLDER,
                "models": [{"id": "m1"}, {"id": "m2"}],
                "headers": {"x-api-key": PLACEHOLDER}
            })
        );
        assert_eq!(
            Value::Object(a.managed.settings.clone()),
            json!({"defaultProvider": "gw", "defaultModel": "m1"})
        );
        for route in a.routes {
            route.resolve(false).unwrap();
        }
    }

    #[test]
    fn a_pasted_subscription_token_takes_the_sign_in_route() {
        let store = AbsPath::new("/s/keys.json").unwrap();
        let mut keys = KeyStore::default();
        keys.insert("anthropic", "sk-ant-oat01-test-secret-abcd");
        let a = additions(&Stored::default(), &keys, &store, &ServerConfig::default());
        let route = &a.routes[0];
        assert_eq!(a.routes.len(), 1);
        assert_eq!(
            (
                route.provider.as_str(),
                route.upstream.as_str(),
                route.inject_header.as_deref(),
                route.credential_format.as_deref(),
                route.allow.as_slice(),
            ),
            (
                "anthropic",
                "https://api.anthropic.com",
                Some("Authorization"),
                Some("Bearer {}"),
                ["POST /v1/messages".to_owned()].as_slice(),
            )
        );
        route.resolve(false).unwrap();
        assert_eq!(a.managed.key_providers, ["anthropic"]);
        assert_eq!(
            a.managed.stand_ins["anthropic"],
            format!("sk-ant-oat01-{PLACEHOLDER}")
        );
        assert!(a.claims.is_empty());

        // An API key keeps the x-api-key route and the plain placeholder.
        keys.insert("anthropic", "sk-ant-api03-test-secret");
        let a = additions(&Stored::default(), &keys, &store, &ServerConfig::default());
        assert_eq!(a.routes[0].inject_header.as_deref(), Some("x-api-key"));
        assert!(a.managed.stand_ins.is_empty());
    }

    #[test]
    fn a_server_declared_gateway_name_wins() {
        let mut stored = Stored::default();
        stored.providers.insert(
            "gw".into(),
            StoredProvider {
                key: key(),
                gateway: Some(gateway()),
                test: None,
            },
        );
        let mut keys = KeyStore::default();
        keys.insert("gw", "secret");
        let server = ServerConfig {
            models: json!({"gw": {"baseUrl": "https://other.example.com"}})
                .as_object()
                .unwrap()
                .clone(),
            ..ServerConfig::default()
        };
        let store = AbsPath::new("/s/keys.json").unwrap();
        let a = additions(&stored, &keys, &store, &server);
        assert!(a.routes.is_empty() && a.managed.models.is_empty());
        assert!(a.managed.key_providers.is_empty());
    }

    #[test]
    fn base_urls() {
        assert_eq!(
            split_base_url("https://gw.example.com/v1/").unwrap(),
            ("https://gw.example.com".into(), "/v1".into())
        );
        assert_eq!(
            split_base_url("http://127.0.0.1:4000").unwrap(),
            ("http://127.0.0.1:4000".into(), String::new())
        );
        for bad in [
            "gw.example.com",
            "ftp://x/y",
            "https://",
            "https://x/v1?k=1",
            "https://x/*",
        ] {
            assert!(split_base_url(bad).is_err(), "{bad}");
        }
    }
}
