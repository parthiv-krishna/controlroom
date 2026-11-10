//! The providers Settings › Models offers, with the routes their credentials take.
//!
//! Each route was checked against pi 1.2.0's provider implementations (`pi-ai`): the origin pi
//! talks to, the endpoints it calls, and the header its SDK puts the credential in (nono's
//! proxy replaces that header's placeholder with the real credential). Models come from pi,
//! never from here. Only these presets and explicit custom gateways get routes from the UI, so
//! the sandbox's network allow-list stays deliberate.

use controlroom_protocol::agentd::{AuthHeader, KeyKind};

use crate::config::sandbox::RouteConfig;

/// Where a credential goes and in which header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteSpec {
    /// The origin.
    pub upstream: &'static str,
    /// `METHOD /path-glob` rules sessions may use the credential for.
    pub allow: &'static [&'static str],
    /// The header nono puts the credential in.
    pub header: &'static str,
    /// Its value template.
    pub format: &'static str,
}

impl RouteSpec {
    /// The route for `provider`.
    pub fn route(&self, provider: &str) -> RouteConfig {
        RouteConfig {
            provider: provider.to_owned(),
            upstream: self.upstream.to_owned(),
            allow: self.allow.iter().map(|r| (*r).to_owned()).collect(),
            approve: Vec::new(),
            inject_header: Some(self.header.to_owned()),
            credential_format: Some(self.format.to_owned()),
            cache_ttl_secs: crate::config::sandbox::DEFAULT_CACHE_TTL_SECS,
        }
    }
}

/// A cheap authenticated request that tells whether a key works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// The path, on the route's upstream (a `GET`).
    pub path: &'static str,
    /// Extra headers the API requires.
    pub headers: &'static [(&'static str, &'static str)],
}

/// Access by subscription (pi's `/login`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subscription {
    /// The route of the access token.
    pub route: RouteSpec,
    /// JWT claims the session's synthetic token keeps (`oauth_claims`).
    pub claims: &'static [&'static str],
    /// Long-lived subscription tokens pasted in place of an API key, if the provider has them.
    pub pasted: Option<PastedToken>,
}

/// A subscription token pasted in place of an API key (e.g. from `claude setup-token`). It
/// takes the subscription's route and is used as is: there is nothing to refresh it with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PastedToken {
    /// The prefix pi tells these tokens from API keys by.
    pub prefix: &'static str,
    /// Its test.
    pub probe: Probe,
}

/// A provider Settings › Models offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    /// pi's provider id.
    pub id: &'static str,
    /// Display name.
    pub label: &'static str,
    /// Access with an API key, and its test.
    pub api_key: Option<(RouteSpec, Probe)>,
    /// Access with a subscription.
    pub subscription: Option<Subscription>,
}

const BEARER: &str = "Bearer {}";
const RAW: &str = "{}";

/// pi's Anthropic Messages API: the SDK sends API keys as `x-api-key` and subscription tokens
/// (which pi recognizes by their `sk-ant-oat` prefix) as `Authorization: Bearer`.
const ANTHROPIC_MESSAGES: &[&str] = &["POST /v1/messages"];

/// The headers pi 1.2.0 sends with an Anthropic subscription token besides `Authorization`
/// (pi-ai `api/anthropic-messages.js`: `createClient` and `getBetaFeatures`), so a test looks
/// like a session's requests.
const ANTHROPIC_SUBSCRIPTION_HEADERS: &[(&str, &str)] = &[
    ("anthropic-version", "2023-06-01"),
    ("anthropic-beta", "claude-code-20250219,oauth-2025-04-20"),
    ("user-agent", "claude-cli/2.1.280"),
    ("x-app", "cli"),
];

/// The presets, in display order.
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "anthropic",
        label: "Anthropic",
        api_key: Some((
            RouteSpec {
                upstream: "https://api.anthropic.com",
                allow: ANTHROPIC_MESSAGES,
                header: "x-api-key",
                format: RAW,
            },
            Probe {
                path: "/v1/models",
                headers: &[("anthropic-version", "2023-06-01")],
            },
        )),
        subscription: Some(Subscription {
            route: RouteSpec {
                upstream: "https://api.anthropic.com",
                allow: ANTHROPIC_MESSAGES,
                header: "Authorization",
                format: BEARER,
            },
            claims: &[],
            // `claude setup-token`'s tokens. The model list answers them as it answers keys.
            pasted: Some(PastedToken {
                prefix: "sk-ant-oat",
                probe: Probe {
                    path: "/v1/models",
                    headers: ANTHROPIC_SUBSCRIPTION_HEADERS,
                },
            }),
        }),
    },
    Preset {
        id: "openai-codex",
        label: "ChatGPT (Codex)",
        api_key: None,
        subscription: Some(Subscription {
            route: RouteSpec {
                upstream: "https://chatgpt.com",
                allow: &["POST /backend-api/codex/responses"],
                header: "Authorization",
                format: BEARER,
            },
            // pi reads the account id from the token.
            claims: &["https://api.openai.com/auth"],
            pasted: None,
        }),
    },
    Preset {
        id: "openai",
        label: "OpenAI",
        // pi's OpenAI provider uses the Responses API.
        api_key: Some((
            RouteSpec {
                upstream: "https://api.openai.com",
                allow: &["POST /v1/responses"],
                header: "Authorization",
                format: BEARER,
            },
            Probe {
                path: "/v1/models",
                headers: &[],
            },
        )),
        subscription: None,
    },
    Preset {
        id: "google",
        label: "Google (Gemini)",
        // `@google/genai` sends the key as `x-goog-api-key` and streams with
        // `POST /v1beta/models/<model>:streamGenerateContent`.
        api_key: Some((
            RouteSpec {
                upstream: "https://generativelanguage.googleapis.com",
                allow: &["POST /v1beta/models/*"],
                header: "x-goog-api-key",
                format: RAW,
            },
            Probe {
                path: "/v1beta/models",
                headers: &[],
            },
        )),
        subscription: None,
    },
    Preset {
        id: "openrouter",
        label: "OpenRouter",
        // Chat completions, the Anthropic-compatible messages some models use, and the
        // per-key model list pi personalizes its catalog with.
        api_key: Some((
            RouteSpec {
                upstream: "https://openrouter.ai",
                allow: &[
                    "POST /api/v1/chat/completions",
                    "POST /api/v1/messages",
                    "GET /api/v1/models/user",
                ],
                header: "Authorization",
                format: BEARER,
            },
            // The key's own info: fails without a valid key.
            Probe {
                path: "/api/v1/key",
                headers: &[],
            },
        )),
        subscription: None,
    },
];

/// The preset `id`.
pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// The subscription tokens `id` takes in place of an API key.
pub fn pasted_token(id: &str) -> Option<PastedToken> {
    preset(id)?.subscription?.pasted
}

/// What `key` is as `provider`'s key: a subscription token when it has the prefix of the ones
/// the provider takes.
pub fn key_kind(provider: &str, key: &str) -> KeyKind {
    match pasted_token(provider) {
        Some(token) if key.starts_with(token.prefix) => KeyKind::SubscriptionToken,
        _ => KeyKind::ApiKey,
    }
}

/// [`key_kind`], refusing another preset's subscription token: no other provider takes it, and
/// pi would send it as an API key.
pub fn check_key(provider: &str, key: &str) -> Result<KeyKind, String> {
    for p in PRESETS {
        if let Some(token) = p.subscription.and_then(|s| s.pasted)
            && p.id != provider
            && key.starts_with(token.prefix)
        {
            return Err(format!(
                "{}… is a subscription token for {}; add it to {} instead",
                token.prefix, p.label, p.label
            ));
        }
    }
    Ok(key_kind(provider, key))
}

/// A custom gateway's header and value template.
pub fn gateway_header(auth: AuthHeader) -> Option<(&'static str, &'static str)> {
    match auth {
        AuthHeader::Bearer => Some(("Authorization", BEARER)),
        AuthHeader::XApiKey => Some(("x-api-key", RAW)),
        AuthHeader::ApiKey => Some(("api-key", RAW)),
        AuthHeader::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_route_is_valid() {
        for p in PRESETS {
            let routes = p
                .api_key
                .iter()
                .map(|(route, _)| route)
                .chain(p.subscription.iter().map(|s| &s.route));
            for route in routes {
                let resolved = route.route(p.id).resolve(false).unwrap();
                assert!(!resolved.endpoints.allow.is_empty(), "{}", p.id);
            }
            assert!(p.api_key.is_some() || p.subscription.is_some());
        }
        assert_eq!(preset("anthropic").unwrap().label, "Anthropic");
        assert!(preset("gateway").is_none());
    }

    #[test]
    fn subscription_tokens_are_told_by_their_prefix() {
        let token = "sk-ant-oat01-test-abcd";
        assert_eq!(key_kind("anthropic", token), KeyKind::SubscriptionToken);
        assert_eq!(
            check_key("anthropic", token),
            Ok(KeyKind::SubscriptionToken)
        );
        for key in [
            "sk-ant-api03-test-abcd",
            "sk-ant-test",
            "x-sk-ant-oat01-test",
        ] {
            assert_eq!(check_key("anthropic", key), Ok(KeyKind::ApiKey), "{key}");
        }
        // Another provider's or a gateway's: refused, naming where it goes.
        for provider in ["openai", "openrouter", "google", "my-gateway"] {
            let err = check_key(provider, token).unwrap_err();
            assert!(err.contains("add it to Anthropic"), "{err}");
            assert_eq!(key_kind(provider, token), KeyKind::ApiKey);
        }
        assert_eq!(check_key("openai", "sk-test"), Ok(KeyKind::ApiKey));
        // The token's route is the sign-in's.
        let sub = preset("anthropic").unwrap().subscription.unwrap();
        assert_eq!(
            (sub.route.header, sub.route.format),
            ("Authorization", BEARER)
        );
    }
}
