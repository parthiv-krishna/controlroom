//! What Settings › Models stores, in `<state>/models/` (mode 0700):
//!
//! - `providers.json`: the providers managed here (key metadata, gateways), the last test of
//!   each provider and the defaults of new sessions. No secrets.
//! - `keys.json` (0600): the API keys ([`KeyStore`]). Read only by the agent daemon's user, when
//!   a key is stored and by `controlroom-agentd credential`; no API returns a key.
//!
//! These files are the agent daemon's own: the NixOS module's per-start copies of pi's
//! `models.json` and `settings.json` never touch them.

use std::collections::BTreeMap;

use controlroom_protocol::agentd::{Gateway, ModelDefaults, ProviderTest, StoredKey};
use controlroom_sandbox::AbsPath;
use controlroom_sandbox::credential::KeyStore;
use serde::{Deserialize, Serialize};

use crate::dirs::{create_private_dir, write_private_file};

/// One provider's entry in `providers.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredProvider {
    /// The API key's description (the key is in `keys.json`).
    pub key: Option<StoredKey>,
    /// A custom gateway's settings.
    pub gateway: Option<Gateway>,
    /// The last test (of any provider, the server's too).
    pub test: Option<ProviderTest>,
}

impl StoredProvider {
    /// Whether nothing is left to keep.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// The contents of `providers.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stored {
    /// By pi provider id.
    pub providers: BTreeMap<String, StoredProvider>,
    /// The defaults of new sessions.
    pub defaults: ModelDefaults,
}

impl Stored {
    /// Sets each stored key's kind from the key itself, so keys stored before kinds existed (or
    /// before a provider took subscription tokens) are described as what they are.
    pub fn classify_keys(&mut self, keys: &KeyStore) {
        for (id, provider) in &mut self.providers {
            if let (Some(described), Some(key)) = (&mut provider.key, keys.get(id)) {
                described.kind = super::presets::key_kind(id, key.expose());
            }
        }
    }
}

/// The files of `<state>/models/`.
#[derive(Debug, Clone)]
pub struct Store {
    dir: AbsPath,
}

impl Store {
    /// The store in `dir`.
    pub fn new(dir: AbsPath) -> Self {
        Self { dir }
    }

    /// `keys.json`.
    pub fn keys_path(&self) -> AbsPath {
        self.dir.join("keys.json")
    }

    fn providers_path(&self) -> AbsPath {
        self.dir.join("providers.json")
    }

    /// Reads `providers.json`; missing is empty.
    pub fn load(&self) -> Result<Stored, String> {
        let path = self.providers_path();
        match std::fs::read(path.as_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{path}: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Stored::default()),
            Err(e) => Err(format!("{path}: {e}")),
        }
    }

    /// Writes `providers.json`.
    pub fn save(&self, stored: &Stored) -> Result<(), String> {
        self.create()?;
        let path = self.providers_path();
        let bytes = serde_json::to_vec_pretty(stored).map_err(|e| e.to_string())?;
        write_private_file(&path, &bytes).map_err(|e| format!("{path}: {e}"))
    }

    /// Reads `keys.json`.
    pub fn keys(&self) -> Result<KeyStore, String> {
        KeyStore::load(&self.keys_path()).map_err(|e| e.to_string())
    }

    /// Writes `keys.json`.
    pub fn save_keys(&self, keys: &KeyStore) -> Result<(), String> {
        self.create()?;
        keys.save(&self.keys_path()).map_err(|e| e.to_string())
    }

    /// The directory for pi's view of the providers ([`super::catalog`]).
    pub fn catalog_dir(&self) -> AbsPath {
        self.dir.join("catalog")
    }

    fn create(&self) -> Result<(), String> {
        create_private_dir(&self.dir).map_err(|e| format!("{}: {e}", self.dir))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use controlroom_protocol::UnixMillis;
    use controlroom_protocol::agentd::KeyKind;

    use super::*;

    fn described(kind: KeyKind, last4: &str) -> StoredProvider {
        StoredProvider {
            key: Some(StoredKey {
                kind,
                last4: last4.into(),
                added_at: UnixMillis(1),
                added_by: None,
            }),
            ..StoredProvider::default()
        }
    }

    #[test]
    fn subscription_tokens_are_marked_and_stay_in_the_key_store() {
        let t = tempfile::tempdir().unwrap();
        let store = Store::new(AbsPath::from_path(t.path()).unwrap().join("models"));
        let mut keys = KeyStore::default();
        keys.insert("anthropic", "sk-ant-oat01-test-abcd");
        store.save_keys(&keys).unwrap();
        let mut stored = Stored::default();
        stored.providers.insert(
            "anthropic".into(),
            described(KeyKind::SubscriptionToken, "abcd"),
        );
        store.save(&stored).unwrap();
        let metadata = std::fs::read_to_string(t.path().join("models/providers.json")).unwrap();
        assert!(
            metadata.contains(r#""kind": "subscription_token""#),
            "{metadata}"
        );
        assert!(!metadata.contains("sk-ant"), "{metadata}");
        let secrets = std::fs::read_to_string(t.path().join("models/keys.json")).unwrap();
        assert!(secrets.contains("sk-ant-oat01-test-abcd"));
    }

    #[test]
    fn kinds_follow_the_stored_key() {
        // providers.json from before kinds existed: the kind defaults to an API key.
        let mut stored: Stored = serde_json::from_str(
            r#"{"providers": {
                "anthropic": {"key": {"last4": "abcd", "added_at": 1, "added_by": null}},
                "openai": {"key": {"last4": "9999", "added_at": 1, "added_by": null}}
            }}"#,
        )
        .unwrap();
        assert_eq!(
            stored.providers["anthropic"],
            described(KeyKind::ApiKey, "abcd")
        );
        let mut keys = KeyStore::default();
        keys.insert("anthropic", "sk-ant-oat01-test-abcd");
        keys.insert("openai", "sk-test-9999");
        stored.classify_keys(&keys);
        assert_eq!(
            stored.providers["anthropic"],
            described(KeyKind::SubscriptionToken, "abcd")
        );
        assert_eq!(
            stored.providers["openai"],
            described(KeyKind::ApiKey, "9999")
        );
    }

    #[test]
    fn files_are_private_and_metadata_has_no_secret() {
        let t = tempfile::tempdir().unwrap();
        let store = Store::new(AbsPath::from_path(t.path()).unwrap().join("models"));
        assert_eq!(store.load().unwrap(), Stored::default());
        let mut keys = store.keys().unwrap();
        keys.insert("anthropic", "sk-ant-test-1234");
        store.save_keys(&keys).unwrap();
        let mut stored = Stored::default();
        stored.providers.insert(
            "anthropic".into(),
            StoredProvider {
                key: Some(StoredKey {
                    kind: KeyKind::ApiKey,
                    last4: "1234".into(),
                    added_at: UnixMillis(1),
                    added_by: None,
                }),
                ..StoredProvider::default()
            },
        );
        store.save(&stored).unwrap();
        assert_eq!(store.load().unwrap(), stored);
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&t.path().join("models")), 0o700);
        assert_eq!(mode(&t.path().join("models/keys.json")), 0o600);
        assert_eq!(mode(&t.path().join("models/providers.json")), 0o600);
        let metadata = std::fs::read_to_string(t.path().join("models/providers.json")).unwrap();
        assert!(!metadata.contains("sk-ant"));
    }
}
