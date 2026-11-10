//! The `models.*` service against a scripted models helper and temporary directories.

use controlroom_protocol::ErrorData;
use controlroom_protocol::agentd::{
    AuthHeader, Gateway, KeyKind, LoginStartParams, ModelDefaults, ModelsProviderParams,
    ModelsSetDefaultsParams, ModelsSetGatewayParams, ModelsSetKeyParams, ProviderSource,
};
use controlroom_protocol::ids::DeviceId;
use controlroom_protocol::session::DeviceRef;
use controlroom_sandbox::AbsPath;
use serde_json::json;

use super::Models;
use super::test_support::one_request_server;
use crate::dirs::Dirs;

struct Fixture {
    _tmp: tempfile::TempDir,
    dirs: Dirs,
    models: Models,
}

/// A daemon layout whose server config declares `mock` (a route, a models.json key) and whose
/// helper lists one model per configured provider.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = Dirs::under(AbsPath::from_path(tmp.path()).unwrap());
    let helper = tmp.path().join("models-helper");
    super::test_support::write_executable(
        &helper,
        r#"#!/bin/sh
# list: one model per provider with a credential in the generated auth.json or models.json.
[ "$1" = list ] || exit 2
providers=$(cat "$3/auth.json" "$3/models.json" 2>/dev/null | tr -d ' \n')
printf '{"providers": [{"id": "anthropic", "name": "Anthropic", "oauth": true}], "models": ['
sep=""
for p in anthropic openai mock gw; do
  case "$providers" in *"\"$p\":"*) printf '%s{"provider": "%s", "id": "%s-model", "name": "M", "reasoning": false}' "$sep" "$p" "$p"; sep=",";; esac
done
printf ']}'
"#,
    );
    std::fs::create_dir_all(dirs.config().as_path()).unwrap();
    std::fs::write(
        dirs.sandbox_toml().as_path(),
        format!(
            "models_helper = \"{}\"\noauth_helper = \"/nonexistent\"\n[[routes]]\nprovider = \"mock\"\nupstream = \"https://mock.example.com\"\nallow = [\"POST /v1/chat/completions\"]\n",
            helper.display()
        ),
    )
    .unwrap();
    std::fs::create_dir_all(dirs.pi_agent_dir().as_path()).unwrap();
    std::fs::write(
        dirs.pi_agent_dir().join("models.json").as_path(),
        json!({"providers": {"mock": {"baseUrl": "https://mock.example.com/v1", "apiKey": "!cat key"}}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        dirs.pi_agent_dir().join("settings.json").as_path(),
        json!({"defaultProvider": "mock", "defaultModel": "mock-model"}).to_string(),
    )
    .unwrap();
    Fixture {
        models: Models::new(dirs.clone()),
        dirs,
        _tmp: tmp,
    }
}

fn laptop() -> Option<DeviceRef> {
    Some(DeviceRef {
        device_id: DeviceId::new("laptop-1"),
        label: "Laptop".into(),
    })
}

fn key(provider: &str, key: &str) -> ModelsSetKeyParams {
    ModelsSetKeyParams {
        provider: provider.into(),
        key: key.into(),
        by: laptop(),
    }
}

fn provider(provider: &str) -> ModelsProviderParams {
    ModelsProviderParams {
        provider: provider.into(),
        by: laptop(),
    }
}

#[tokio::test]
async fn keys_are_stored_privately_and_never_returned() {
    let f = fixture();
    let state = f
        .models
        .set_key(key("anthropic", "sk-ant-secret-4f2a"))
        .await
        .unwrap();
    let text = serde_json::to_string(&state).unwrap();
    assert!(!text.contains("sk-ant-secret"), "{text}");
    let row = state
        .providers
        .iter()
        .find(|p| p.id == "anthropic")
        .unwrap();
    assert_eq!(row.source, ProviderSource::Ui);
    let stored = row.key.as_ref().unwrap();
    assert_eq!(stored.last4, "4f2a");
    assert_eq!(stored.added_by.as_ref().unwrap().label, "Laptop");
    // pi now offers the provider's models.
    assert!(state.models.iter().any(|m| m.provider == "anthropic"));

    // Replace, then remove.
    let state = f
        .models
        .set_key(key("anthropic", "sk-ant-other-9999"))
        .await
        .unwrap();
    let row = state
        .providers
        .iter()
        .find(|p| p.id == "anthropic")
        .unwrap();
    assert_eq!(row.key.as_ref().unwrap().last4, "9999");
    let state = f.models.remove(provider("anthropic")).await.unwrap();
    let row = state
        .providers
        .iter()
        .find(|p| p.id == "anthropic")
        .unwrap();
    assert!(row.key.is_none() && row.source == ProviderSource::None);
    let keys = std::fs::read_to_string(f.dirs.models().join("keys.json").as_path()).unwrap();
    assert!(!keys.contains("sk-ant"));
    assert!(f.models.remove(provider("anthropic")).await.is_err());

    // Bad input.
    for bad in ["", "two words", "line\nbreak"] {
        assert!(
            f.models.set_key(key("openai", bad)).await.is_err(),
            "{bad:?}"
        );
    }
    let err = f
        .models
        .set_key(key("not-a-preset", "k"))
        .await
        .unwrap_err();
    assert_eq!(err.data, Some(ErrorData::InvalidParams));
}

#[tokio::test]
async fn the_server_config_wins() {
    let f = fixture();
    let state = f.models.list().await.unwrap();
    let mock = state.providers.iter().find(|p| p.id == "mock").unwrap();
    assert_eq!(mock.source, ProviderSource::Server);
    assert!(!mock.takes_key);
    assert_eq!(state.server_defaults.provider.as_deref(), Some("mock"));
    // A gateway or key under the server's name is refused.
    let err = f.models.set_key(key("mock", "k")).await.unwrap_err();
    assert!(
        err.message.contains("server's configuration"),
        "{}",
        err.message
    );
    let gateway = ModelsSetGatewayParams {
        provider: "mock".into(),
        gateway: Gateway {
            base_url: "https://gw.example.com/v1".into(),
            models: vec!["m".into()],
            auth_header: AuthHeader::Bearer,
        },
        key: Some("k".into()),
        by: laptop(),
    };
    assert!(f.models.set_gateway(gateway.clone()).await.is_err());

    // A gateway stored before the server took its name shows the clash and is ignored.
    let ok = ModelsSetGatewayParams {
        provider: "gw".into(),
        ..gateway
    };
    f.models.set_gateway(ok).await.unwrap();
    let models = json!({"providers": {
        "mock": {"baseUrl": "https://mock.example.com/v1", "apiKey": "!cat key"},
        "gw": {"baseUrl": "https://other.example.com/v1"}
    }});
    std::fs::write(
        f.dirs.pi_agent_dir().join("models.json").as_path(),
        models.to_string(),
    )
    .unwrap();
    let state = f.models.list().await.unwrap();
    let gw = state.providers.iter().find(|p| p.id == "gw").unwrap();
    assert_eq!(gw.source, ProviderSource::Server);
    assert!(gw.clash.as_deref().unwrap().contains("models.json"));
}

#[tokio::test]
async fn gateways_and_defaults() {
    let f = fixture();
    let gateway = |base_url: &str, key: Option<&str>| ModelsSetGatewayParams {
        provider: "gw".into(),
        gateway: Gateway {
            base_url: base_url.into(),
            models: vec!["gw-model".into(), " gw-model ".into()],
            auth_header: AuthHeader::XApiKey,
        },
        key: key.map(str::to_owned),
        by: laptop(),
    };
    // New gateways need a key; loopback isn't allowed here; names are checked.
    assert!(
        f.models
            .set_gateway(gateway("https://gw.example.com/v1", None))
            .await
            .is_err()
    );
    assert!(
        f.models
            .set_gateway(gateway("http://127.0.0.1:4000/v1", Some("k")))
            .await
            .is_err()
    );
    let mut bad_name = gateway("https://gw.example.com/v1", Some("k"));
    bad_name.provider = "Anthropic!".into();
    assert!(f.models.set_gateway(bad_name).await.is_err());

    let state = f
        .models
        .set_gateway(gateway("https://gw.example.com/v1", Some("gw-secret")))
        .await
        .unwrap();
    let gw = state.providers.iter().find(|p| p.id == "gw").unwrap();
    assert_eq!(gw.gateway.as_ref().unwrap().models, ["gw-model"]);
    assert!(state.models.iter().any(|m| m.provider == "gw"));
    // Editing keeps the key.
    let state = f
        .models
        .set_gateway(gateway("https://gw2.example.com/v1", None))
        .await
        .unwrap();
    let gw = state.providers.iter().find(|p| p.id == "gw").unwrap();
    assert_eq!(gw.key.as_ref().unwrap().last4, "cret");

    let defaults = |provider: Option<&str>, model: Option<&str>, thinking: Option<&str>| {
        ModelsSetDefaultsParams {
            defaults: ModelDefaults {
                provider: provider.map(str::to_owned),
                model: model.map(str::to_owned),
                thinking: thinking.map(str::to_owned),
            },
            by: laptop(),
        }
    };
    assert!(
        f.models
            .set_defaults(defaults(None, Some("m"), None))
            .await
            .is_err()
    );
    assert!(
        f.models
            .set_defaults(defaults(Some("gw"), None, Some("extreme")))
            .await
            .is_err()
    );
    let state = f
        .models
        .set_defaults(defaults(Some("gw"), Some("gw-model"), Some("high")))
        .await
        .unwrap();
    assert_eq!(state.defaults.model.as_deref(), Some("gw-model"));

    // A launch gets the gateway's route and models.json entry, the key store and the defaults.
    let mut sandbox = crate::config::sandbox::load(&f.dirs)
        .value
        .resolve(None, &f.dirs.pi_agent_dir())
        .unwrap();
    super::merge::apply(&f.dirs, &mut sandbox);
    let routes: Vec<_> = sandbox.routes.iter().map(|r| r.provider.as_str()).collect();
    assert_eq!(routes, ["mock", "gw"]);
    let managed = &sandbox.agent_dir.managed;
    assert_eq!(managed.key_providers, ["gw"]);
    assert_eq!(managed.settings["defaultModel"], "gw-model");
    assert_eq!(managed.settings["defaultThinkingLevel"], "high");
}

#[tokio::test]
async fn sign_in_needs_a_provider_pi_can_sign_in_to() {
    let f = fixture();
    let start = |p: &str| LoginStartParams {
        provider: p.into(),
        by: laptop(),
    };
    let err = f.models.login_start(start("openai")).await.unwrap_err();
    assert_eq!(err.data, Some(ErrorData::InvalidParams));
    f.models
        .set_key(key("anthropic", "sk-ant-k"))
        .await
        .unwrap();
    let err = f.models.login_start(start("anthropic")).await.unwrap_err();
    assert!(err.message.contains("remove it first"), "{}", err.message);
}

/// A pasted subscription token (`claude setup-token`) for Anthropic: stored as a key, marked as
/// a token, tested with Bearer and pi's subscription headers, refused elsewhere, and exclusive
/// with a sign-in.
#[tokio::test]
async fn anthropic_subscription_tokens() {
    let f = fixture();
    let token = "sk-ant-oat01-test-good-abcd";
    let state = f.models.set_key(key("anthropic", token)).await.unwrap();
    assert!(!serde_json::to_string(&state).unwrap().contains(token));
    let row = state
        .providers
        .iter()
        .find(|p| p.id == "anthropic")
        .unwrap();
    let stored = row.key.as_ref().unwrap();
    assert_eq!(
        (stored.kind, stored.last4.as_str()),
        (KeyKind::SubscriptionToken, "abcd")
    );
    assert_eq!(row.token_prefix.as_deref(), Some("sk-ant-oat"));
    let openai = state.providers.iter().find(|p| p.id == "openai").unwrap();
    assert_eq!(openai.token_prefix, None);
    // pi offers Anthropic's models with the token's stand-in.
    assert!(state.models.iter().any(|m| m.provider == "anthropic"));

    // The test: the model list, Bearer and the headers pi sends with subscription tokens.
    let ctx = f.models.context().unwrap();
    let mut request = super::probe_target(&ctx, "anthropic").unwrap().unwrap();
    assert_eq!(request.url, "https://api.anthropic.com/v1/models");
    let (origin, task) = one_request_server("authorization: bearer sk-ant-oat01-test-good").await;
    request.url = format!("{origin}/v1/models");
    assert_eq!(
        super::probe::probe(&request, token).await.unwrap(),
        "HTTP 200 OK"
    );
    let sent = task.await.unwrap();
    for header in [
        "anthropic-version: 2023-06-01",
        "anthropic-beta: claude-code-20250219,oauth-2025-04-20",
        "user-agent: claude-cli/",
        "x-app: cli",
    ] {
        assert!(sent.contains(header), "{header}: {sent}");
    }
    assert!(!sent.contains("x-api-key"), "{sent}");
    // An expired or revoked token says so.
    let (origin, _task) = one_request_server("authorization: bearer sk-ant-oat01-test-good").await;
    request.url = format!("{origin}/v1/models");
    assert_eq!(
        super::probe::probe(&request, "sk-ant-oat01-test-revoked")
            .await
            .unwrap_err(),
        "HTTP 401: token rejected (expired or revoked?)"
    );

    // Elsewhere it's refused, naming where it goes.
    let err = f.models.set_key(key("openai", token)).await.unwrap_err();
    assert_eq!(err.data, Some(ErrorData::InvalidParams));
    assert!(
        err.message.contains("add it to Anthropic"),
        "{}",
        err.message
    );
    let gateway = ModelsSetGatewayParams {
        provider: "gw".into(),
        gateway: Gateway {
            base_url: "https://gw.example.com/v1".into(),
            models: vec!["m".into()],
            auth_header: AuthHeader::XApiKey,
        },
        key: Some(token.into()),
        by: laptop(),
    };
    let err = f.models.set_gateway(gateway).await.unwrap_err();
    assert!(
        err.message.contains("add it to Anthropic"),
        "{}",
        err.message
    );

    // A sign-in and a token exclude each other, as with keys.
    let start = LoginStartParams {
        provider: "anthropic".into(),
        by: laptop(),
    };
    let err = f.models.login_start(start).await.unwrap_err();
    assert!(err.message.contains("remove it first"), "{}", err.message);
    f.models.remove(provider("anthropic")).await.unwrap();
    std::fs::write(
        f.dirs.pi_agent_dir().join("auth.json").as_path(),
        json!({"anthropic": {"type": "oauth", "access": "sk-ant-oat01-x", "refresh": "r", "expires": 1}})
            .to_string(),
    )
    .unwrap();
    let err = f.models.set_key(key("anthropic", token)).await.unwrap_err();
    assert!(err.message.contains("sign out first"), "{}", err.message);
}

/// A token stored before kinds existed is recognized from the key itself.
#[tokio::test]
async fn stored_tokens_are_reclassified() {
    let f = fixture();
    std::fs::create_dir_all(f.dirs.models().as_path()).unwrap();
    std::fs::write(
        f.dirs.models().join("keys.json").as_path(),
        json!({"anthropic": "sk-ant-oat01-test-abcd"}).to_string(),
    )
    .unwrap();
    std::fs::write(
        f.dirs.models().join("providers.json").as_path(),
        json!({"providers": {"anthropic": {"key": {"last4": "abcd", "added_at": 1, "added_by": null}}}})
            .to_string(),
    )
    .unwrap();
    let state = f.models.list().await.unwrap();
    let row = state
        .providers
        .iter()
        .find(|p| p.id == "anthropic")
        .unwrap();
    assert_eq!(row.key.as_ref().unwrap().kind, KeyKind::SubscriptionToken);
    let ctx = f.models.context().unwrap();
    let request = super::probe_target(&ctx, "anthropic").unwrap().unwrap();
    assert_eq!(request.header, "Authorization");
}
