//! The key store: API keys added in Settings › Models, outside the user's pi config.
//!
//! One JSON object, provider id → key, in a 0600 file only the agent daemon's user reads (its
//! state dir is 0700 too). The agent daemon writes it; `controlroom-agentd credential` reads one
//! key for nono's credential capture. Keys never enter a session's agent dir: sessions get
//! placeholders.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{CredentialError, Secret};
use crate::fsutil::{PathIoError, write_atomic};
use crate::paths::AbsPath;

/// Where a session's stored keys are, for the credential helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredKeys {
    /// The key store file.
    pub path: AbsPath,
    /// The providers whose key the session takes from it.
    pub providers: Vec<String>,
}

/// The keys of a key store file. `Debug` doesn't print them.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyStore(BTreeMap<String, String>);

impl std::fmt::Debug for KeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

impl KeyStore {
    /// Reads `path`; a missing file is an empty store.
    pub fn load(path: &AbsPath) -> Result<Self, CredentialError> {
        let bytes = match std::fs::read(path.as_path()) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(CredentialError::Config {
                    path: path.to_string(),
                    problem: e.to_string(),
                });
            }
        };
        // Do not echo parse errors: they can quote a key.
        serde_json::from_slice(&bytes).map_err(|_| CredentialError::Config {
            path: path.to_string(),
            problem: "not a JSON object of strings".to_owned(),
        })
    }

    /// Writes the store to `path` (mode 0600, atomically).
    pub fn save(&self, path: &AbsPath) -> Result<(), PathIoError> {
        let bytes = serde_json::to_vec_pretty(&self.0).expect("a string map serializes");
        write_atomic(path.as_path(), &bytes, 0o600)
    }

    /// The providers with a key.
    pub fn providers(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Whether `provider` has a key.
    pub fn contains(&self, provider: &str) -> bool {
        self.0.contains_key(provider)
    }

    /// `provider`'s key.
    pub fn get(&self, provider: &str) -> Option<Secret> {
        self.0.get(provider).map(|key| Secret(key.clone()))
    }

    /// Adds or replaces `provider`'s key.
    pub fn insert(&mut self, provider: &str, key: &str) {
        self.0.insert(provider.to_owned(), key.to_owned());
    }

    /// Removes `provider`'s key; whether there was one.
    pub fn remove(&mut self, provider: &str) -> bool {
        self.0.remove(provider).is_some()
    }
}

/// `provider`'s key from the session's stored keys.
pub fn resolve_stored_key(stored: &StoredKeys, provider: &str) -> Result<Secret, CredentialError> {
    if !stored.providers.iter().any(|p| p == provider) {
        return Err(CredentialError::UnknownProvider(provider.to_owned()));
    }
    KeyStore::load(&stored.path)?
        .get(provider)
        .ok_or_else(|| CredentialError::Unresolved(provider.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn round_trip_private_and_redacted() {
        let t = tempfile::tempdir().unwrap();
        let path = AbsPath::from_path(&t.path().join("keys.json")).unwrap();
        assert_eq!(KeyStore::load(&path).unwrap(), KeyStore::default());
        let mut store = KeyStore::default();
        store.insert("anthropic", "sk-test-secret");
        store.save(&path).unwrap();
        let mode = std::fs::metadata(path.as_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let loaded = KeyStore::load(&path).unwrap();
        assert_eq!(loaded.get("anthropic").unwrap().expose(), "sk-test-secret");
        assert!(!format!("{loaded:?}").contains("sk-test"));

        let stored = StoredKeys {
            path: path.clone(),
            providers: vec!["anthropic".into()],
        };
        assert_eq!(
            resolve_stored_key(&stored, "anthropic").unwrap().expose(),
            "sk-test-secret"
        );
        assert!(matches!(
            resolve_stored_key(&stored, "openai"),
            Err(CredentialError::UnknownProvider(_))
        ));
        store.remove("anthropic");
        store.save(&path).unwrap();
        assert!(matches!(
            resolve_stored_key(&stored, "anthropic"),
            Err(CredentialError::Unresolved(_))
        ));

        std::fs::write(path.as_path(), "{\"anthropic\": 4").unwrap();
        let err = KeyStore::load(&path).unwrap_err().to_string();
        assert!(!err.contains("anthropic"), "{err}");
    }
}
