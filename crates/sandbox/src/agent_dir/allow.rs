//! A session's model allow-list: its profile's model policy (design
//! `docs/design/profiles-tenants.md` §2, A1, A4 and A6). pi only learns of the providers on the
//! list: the others are left out of the session's `models.json`, `auth.json` and default
//! provider, so pi neither lists nor accepts their models, and with no credential they get no
//! route and no entry in the credential manifest.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

/// The providers (accounts) a session may use. A session without one may use every configured
/// provider.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderAllowList(BTreeSet<String>);

impl ProviderAllowList {
    /// The list of `providers` (pi provider ids, accounts included). An empty list admits none.
    pub fn new<I, S>(providers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self(providers.into_iter().map(Into::into).collect())
    }

    /// Whether `provider` is on the list.
    pub fn allows(&self, provider: &str) -> bool {
        self.0.contains(provider)
    }
}

/// Whether `allowed` (none: no list) admits `provider`.
pub(super) fn admits(allowed: Option<&ProviderAllowList>, provider: &str) -> bool {
    allowed.is_none_or(|list| list.allows(provider))
}

/// Removes the providers `allowed` doesn't admit from a `models.json` document.
pub(super) fn retain_models(models: &mut Map<String, Value>, allowed: Option<&ProviderAllowList>) {
    if let Some(Value::Object(providers)) = models.get_mut("providers") {
        providers.retain(|provider, _| admits(allowed, provider));
    }
}

/// Drops the default provider and model from `settings` when `allowed` doesn't admit the
/// provider (pi then starts on the first model it has a credential for).
pub(super) fn retain_default(
    settings: &mut Map<String, Value>,
    allowed: Option<&ProviderAllowList>,
) {
    let refused = settings
        .get("defaultProvider")
        .and_then(Value::as_str)
        .is_some_and(|provider| !admits(allowed, provider));
    if refused {
        settings.remove("defaultProvider");
        settings.remove("defaultModel");
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => panic!("not an object"),
        }
    }

    #[test]
    fn only_listed_providers_stay() {
        let list = ProviderAllowList::new(["local"]);
        let mut models = object(json!({"providers": {"local": {}, "paid": {}}}));
        retain_models(&mut models, Some(&list));
        assert_eq!(Value::Object(models), json!({"providers": {"local": {}}}));

        let mut models = object(json!({"providers": {"local": {}, "paid": {}}}));
        retain_models(&mut models, Some(&ProviderAllowList::default()));
        assert_eq!(Value::Object(models), json!({"providers": {}}));

        let all = json!({"providers": {"local": {}, "paid": {}}});
        let mut models = object(all.clone());
        retain_models(&mut models, None);
        assert_eq!(Value::Object(models), all);
    }

    #[test]
    fn a_refused_default_is_dropped_with_its_model() {
        let list = ProviderAllowList::new(["local"]);
        let mut settings = object(json!({"defaultProvider": "paid", "defaultModel": "m", "x": 1}));
        retain_default(&mut settings, Some(&list));
        assert_eq!(Value::Object(settings), json!({"x": 1}));

        let mut settings = object(json!({"defaultProvider": "local", "defaultModel": "m"}));
        retain_default(&mut settings, Some(&list));
        assert_eq!(settings.len(), 2);
    }
}
