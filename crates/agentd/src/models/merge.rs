//! What Settings › Models adds to a launch: routes, synthetic-token claims, and the session's
//! stored keys (with the stand-ins of pasted subscription tokens), gateways, accounts and
//! defaults ([`ManagedProviders`]). Server-declared providers are left alone (the server's
//! configuration wins on a name clash), so the server's routes, `models.json` entries and keys
//! are never replaced.
//!
//! An added account (e.g. `anthropic-work`) is a provider of its own for pi: a `models.json`
//! entry with its provider's models (pi's definitions, [`Store::definitions`]; a gateway's own),
//! an API-key stand-in in `auth.json`, and its provider's route under its own id, so nono
//! injects that account's credential. A signed-in account's stand-in is a synthetic token
//! keeping the claims pi reads (`synthetic_access`), and its token is refreshed in its own
//! `auth.json` ([`OAuthAccount`]).

use std::collections::BTreeMap;

use controlroom_protocol::agentd::{Gateway, KeyKind};
use controlroom_sandbox::agent_dir::{ManagedProviders, PLACEHOLDER, stand_in, synthetic_access};
use controlroom_sandbox::credential::{KeyStore, OAuthAccount, Secret};
use serde_json::{Map, Value, json};

use super::presets::{self, PRESETS, Preset};
use super::server::ServerConfig;
use super::store::{Definitions, Store, Stored};
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

/// Computes the additions from what `store` holds (`stored`, `keys`, and the accounts'
/// sign-ins and definitions it reads).
pub fn additions(
    stored: &Stored,
    keys: &KeyStore,
    store: &Store,
    server: &ServerConfig,
) -> Additions {
    let mut out = Additions {
        managed: ManagedProviders {
            key_store: Some(store.keys_path()),
            ..ManagedProviders::default()
        },
        ..Additions::default()
    };
    for preset in PRESETS {
        if server.declares(preset.id).is_some() {
            continue;
        }
        if server.auth_type(preset.id) == Some("oauth")
            && let Some(sub) = preset.subscription
        {
            out.routes.push(sub.route.route(preset.id));
            if !sub.claims.is_empty() {
                let claims = sub.claims.iter().map(|c| (*c).to_owned()).collect();
                out.claims.insert(preset.id.to_owned(), claims);
            }
        } else if let Some(key) = keys.get(preset.id) {
            out.add_key(preset, preset.id, &key);
        }
    }
    for (id, provider) in &stored.providers {
        let Some(gateway) = &provider.gateway else {
            continue;
        };
        if server.declares(id).is_some() || !keys.contains(id) {
            continue;
        }
        out.add_gateway(id, gateway, gateway_models_entry(gateway));
    }
    let definitions = store.definitions();
    for (id, account) in &stored.providers {
        let Some(base) = account.account_of.as_deref() else {
            continue;
        };
        if server.declares(id).is_some() {
            continue;
        }
        let name = stored.account_name(id);
        if let Some(gateway) = stored.providers.get(base).and_then(|b| b.gateway.as_ref()) {
            if server.declares(base).is_none() && keys.contains(id) {
                let mut entry = gateway_models_entry(gateway);
                entry["name"] = Value::String(format!("{base} ({name})"));
                out.add_gateway(id, gateway, entry);
            }
        } else if let Some(preset) = presets::preset(base) {
            out.add_preset_account(preset, id, &name, store, keys, &definitions);
        }
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

impl Additions {
    /// A preset's (or a preset account's) stored key: its route, and for a pasted subscription
    /// token the sign-in's route and a stand-in with the token's type prefix, by which pi sends
    /// it as a subscription's.
    fn add_key(&mut self, preset: &Preset, id: &str, key: &Secret) {
        let route = if presets::key_kind(preset.id, key.expose()) == KeyKind::SubscriptionToken
            && let Some(sub) = preset.subscription
        {
            self.managed
                .stand_ins
                .insert(id.to_owned(), stand_in(key.expose()));
            sub.route
        } else if let Some((route, _)) = preset.api_key {
            route
        } else {
            return;
        };
        self.routes.push(route.route(id));
        self.managed.key_providers.push(id.to_owned());
    }

    /// A gateway (or a gateway's account) with its `models.json` entry.
    fn add_gateway(&mut self, id: &str, gateway: &Gateway, entry: Value) {
        let Ok(route) = gateway_route(id, gateway) else {
            return;
        };
        self.routes.push(route);
        self.managed.key_providers.push(id.to_owned());
        self.managed.models.insert(id.to_owned(), entry);
    }

    /// An account added to a preset: its own sign-in or key, and its preset's models.
    fn add_preset_account(
        &mut self,
        preset: &Preset,
        id: &str,
        name: &str,
        store: &Store,
        keys: &KeyStore,
        definitions: &Definitions,
    ) {
        let Some(models) = definitions.get(preset.id).filter(|m| !m.is_empty()) else {
            tracing::warn!(
                "Settings › Models: account {id} skipped: no models of {} known yet (open \
                 Settings › Models to fetch them)",
                preset.id
            );
            return;
        };
        let auth = store.account_auth(id);
        let oauth = auth
            .get(preset.id)
            .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("oauth"));
        if let (Some(entry), Some(sub)) = (oauth, preset.subscription) {
            let claims: Vec<String> = sub.claims.iter().map(|c| (*c).to_owned()).collect();
            let access = entry.get("access").and_then(Value::as_str).unwrap_or("");
            self.routes.push(sub.route.route(id));
            self.managed
                .stand_ins
                .insert(id.to_owned(), synthetic_access(access, &claims));
            self.managed.oauth_accounts.insert(
                id.to_owned(),
                OAuthAccount {
                    agent_dir: store.account_dir(id),
                    provider: preset.id.to_owned(),
                },
            );
        } else if let Some(key) = keys.get(id) {
            self.add_key(preset, id, &key);
        } else {
            return;
        }
        self.managed.models.insert(
            id.to_owned(),
            json!({"name": format!("{} ({name})", preset.label), "models": models}),
        );
    }
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
    let additions = additions(&stored, &keys, &store, &server);
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
    use controlroom_sandbox::AbsPath;
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
                ..Default::default()
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
        let store = Store::new(AbsPath::new("/var/lib/controlroom-agent/models").unwrap());
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
        assert_eq!(a.managed.key_store, Some(store.keys_path()));
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
        let store = Store::new(AbsPath::new("/s").unwrap());
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
                ..Default::default()
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
        let store = Store::new(AbsPath::new("/s").unwrap());
        let a = additions(&stored, &keys, &store, &server);
        assert!(a.routes.is_empty() && a.managed.models.is_empty());
        assert!(a.managed.key_providers.is_empty());
    }

    /// An added account of `base` named `name`.
    fn account(base: &str, name: &str) -> StoredProvider {
        StoredProvider {
            account_of: Some(base.into()),
            name: Some(name.into()),
            ..Default::default()
        }
    }

    #[test]
    fn accounts_get_their_own_route_stand_in_and_sign_in() {
        let t = tempfile::tempdir().unwrap();
        let store = Store::new(AbsPath::from_path(t.path()).unwrap().join("models"));
        let model = json!({"id": "m1", "api": "anthropic-messages", "baseUrl": "https://api.anthropic.com"});
        store
            .save_definitions(&BTreeMap::from([
                ("anthropic".to_owned(), vec![model.clone()]),
                ("openai-codex".to_owned(), vec![json!({"id": "c1"})]),
            ]))
            .unwrap();
        let mut stored = Stored::default();
        stored
            .providers
            .insert("anthropic-work".into(), account("anthropic", "work"));
        stored
            .providers
            .insert("anthropic-side".into(), account("anthropic", "side"));
        stored
            .providers
            .insert("openai-codex-work".into(), account("openai-codex", "work"));
        stored.providers.insert(
            "gw".into(),
            StoredProvider {
                gateway: Some(gateway()),
                ..Default::default()
            },
        );
        stored
            .providers
            .insert("gw-team".into(), account("gw", "team"));
        // Not set up yet: no credential, so not offered to sessions.
        stored
            .providers
            .insert("openai-spare".into(), account("openai", "spare"));
        let mut keys = KeyStore::default();
        keys.insert("anthropic", "sk-ant-api03-personal");
        keys.insert("anthropic-work", "sk-ant-oat01-work-secret");
        keys.insert("anthropic-side", "sk-ant-api03-side");
        keys.insert("gw", "gw-key");
        keys.insert("gw-team", "gw-team-key");
        // The codex account's own sign-in, with the account claim pi reads.
        let claim = json!({"chatgpt_account_id": "acct-work"});
        let access = controlroom_sandbox::agent_dir::unsigned_jwt(
            json!({"https://api.openai.com/auth": claim, "email": "alice@example.com"})
                .as_object()
                .unwrap(),
        );
        let dir = store.create_account_dir("openai-codex-work").unwrap();
        std::fs::write(
            dir.join("auth.json").as_path(),
            json!({"openai-codex": {"type": "oauth", "access": access, "refresh": "r", "expires": 1}})
                .to_string(),
        )
        .unwrap();
        let a = additions(&stored, &keys, &store, &ServerConfig::default());
        let routes: BTreeMap<&str, (&str, &str)> = a
            .routes
            .iter()
            .map(|r| {
                (
                    r.provider.as_str(),
                    (r.upstream.as_str(), r.inject_header.as_deref().unwrap()),
                )
            })
            .collect();
        assert_eq!(
            routes,
            BTreeMap::from([
                ("anthropic", ("https://api.anthropic.com", "x-api-key")),
                // The pasted token takes the subscription route, under the account's id.
                (
                    "anthropic-work",
                    ("https://api.anthropic.com", "Authorization")
                ),
                ("anthropic-side", ("https://api.anthropic.com", "x-api-key")),
                (
                    "openai-codex-work",
                    ("https://chatgpt.com", "Authorization")
                ),
                ("gw", ("https://gw.example.com", "x-api-key")),
                ("gw-team", ("https://gw.example.com", "x-api-key")),
            ])
        );
        for route in &a.routes {
            route.clone().resolve(false).unwrap();
        }
        let m = &a.managed;
        assert_eq!(
            m.key_providers,
            [
                "anthropic",
                "gw",
                "anthropic-side",
                "anthropic-work",
                "gw-team"
            ]
        );
        assert_eq!(
            m.stand_ins["anthropic-work"],
            format!("sk-ant-oat01-{PLACEHOLDER}")
        );
        assert!(!m.stand_ins.contains_key("anthropic-side"));
        // The codex account: its own auth.json, a stand-in with the account claim only.
        assert_eq!(
            m.oauth_accounts["openai-codex-work"],
            OAuthAccount {
                agent_dir: store.account_dir("openai-codex-work"),
                provider: "openai-codex".into()
            }
        );
        let stand_in = &m.stand_ins["openai-codex-work"];
        assert!(!stand_in.contains(&access) && stand_in.ends_with('.'));
        assert_eq!(
            stand_in,
            &synthetic_access(&access, &["https://api.openai.com/auth".to_owned()])
        );
        assert!(
            a.claims.is_empty(),
            "accounts carry their claims in the stand-in"
        );
        // Each account's models are its provider's, under a name naming the account.
        assert_eq!(
            m.models["anthropic-work"],
            json!({"name": "Anthropic (work)", "models": [model]})
        );
        assert_eq!(m.models["gw-team"]["name"], "gw (team)");
        assert_eq!(m.models["gw-team"]["apiKey"], PLACEHOLDER);
        assert!(!m.models.contains_key("openai-spare"));
        assert!(!routes.contains_key("openai-spare"));

        // Without pi's definitions of its provider, an account is skipped.
        store.save_definitions(&BTreeMap::new()).unwrap();
        let a = additions(&stored, &keys, &store, &ServerConfig::default());
        assert!(!a.managed.models.contains_key("anthropic-work"));
        assert!(
            a.routes
                .iter()
                .all(|r| r.provider.as_str() != "anthropic-work")
        );
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
