//! Host-side credential lookup for `controlroomd credential <session> <provider>` (§11.3).
//!
//! nono's `credential_capture` runs `controlroomd credential` unsandboxed and injects its
//! stdout into requests on the provider's route. It reads the session's [`CredentialManifest`]
//! (written next to, not inside, the sandboxed agent dir) and produces the provider's real
//! credential host-side, so it never enters the sandbox:
//!
//! - [`api_key`]: API keys, resolved from the user's real pi config as pi would;
//! - [`oauth`]: OAuth access tokens, refreshed by pi itself through the bundle's helper.

mod api_key;
mod key_store;
mod oauth;
mod process;

use std::fmt;

use serde::{Deserialize, Serialize};

pub use api_key::{models_api_key_providers, resolve_api_key, resolve_config_value};
pub use key_store::{KeyStore, StoredKeys, resolve_stored_key};
pub use oauth::fresh_oauth_token;

use crate::layout::SessionLayout;
use crate::names::Name;
use crate::paths::AbsPath;

/// Written by the agent-dir builder; tells the helper where the real config lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialManifest {
    /// The user's real pi agent dir.
    pub agent_dir: AbsPath,
    /// Providers configured with API keys.
    pub api_key_providers: Vec<String>,
    /// Providers configured with OAuth tokens (in the real `auth.json`).
    #[serde(default)]
    pub oauth_providers: Vec<String>,
    /// The OAuth helper (`controlroom-pi-oauth`), when one is configured.
    #[serde(default)]
    pub oauth_helper: Option<AbsPath>,
    /// API keys added in Settings › Models (the user's own config wins on a clash).
    #[serde(default)]
    pub stored_keys: Option<StoredKeys>,
}

/// A resolved credential. `Debug` does not print it.
pub struct Secret(String);

impl Secret {
    /// The credential. Write it only to the helper's stdout.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Why no credential was produced. Messages never contain key or token material.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// The session has no manifest (not built, or no user agent dir).
    #[error("no credential manifest for this session: {0}")]
    NoManifest(String),
    /// The provider has no credential in this session.
    #[error(
        "provider {0} has no API key or sign-in in this session (Settings › Models, or the \
         server's config)"
    )]
    UnknownProvider(String),
    /// The configured value could not be resolved (missing env var, failing command).
    #[error("the API key of provider {0} could not be resolved")]
    Unresolved(String),
    /// An OAuth provider, but no helper to refresh its token.
    #[error(
        "provider {0} uses OAuth, which needs the controlroom-pi bundle's controlroom-pi-oauth \
         (sandbox.toml `oauth_helper`)"
    )]
    NoOAuthHelper(String),
    /// The OAuth helper failed.
    #[error("the OAuth token of provider {provider} could not be refreshed: {problem}")]
    OAuth {
        /// The provider.
        provider: String,
        /// The helper's message.
        problem: String,
    },
    /// A config file could not be read or parsed.
    #[error("{path}: {problem}")]
    Config {
        /// The file.
        path: String,
        /// What went wrong.
        problem: String,
    },
}

/// Reads the manifest of `layout`'s session.
pub fn load_manifest(layout: &SessionLayout) -> Result<CredentialManifest, CredentialError> {
    let path = layout.credential_manifest();
    let bytes = std::fs::read(path.as_path())
        .map_err(|e| CredentialError::NoManifest(format!("{path}: {e}")))?;
    serde_json::from_slice(&bytes).map_err(|e| CredentialError::Config {
        path: path.to_string(),
        problem: e.to_string(),
    })
}

/// `provider`'s credential: a fresh OAuth access token, valid for at least `valid_for_secs`, if
/// the user's `auth.json` stores OAuth for it (pi prefers a stored credential), else its API
/// key from the user's config, else its key from the key store.
pub fn resolve_credential(
    manifest: &CredentialManifest,
    provider: &Name,
    valid_for_secs: u32,
) -> Result<Secret, CredentialError> {
    if manifest
        .oauth_providers
        .iter()
        .any(|p| p == provider.as_str())
    {
        fresh_oauth_token(manifest, provider, valid_for_secs)
    } else if manifest
        .api_key_providers
        .iter()
        .any(|p| p == provider.as_str())
    {
        resolve_api_key(manifest, provider)
    } else if let Some(stored) = &manifest.stored_keys {
        resolve_stored_key(stored, provider.as_str())
    } else {
        Err(CredentialError::UnknownProvider(provider.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::SessionId;

    #[test]
    fn manifest_round_trip() {
        let t = tempfile::tempdir().unwrap();
        let state = AbsPath::from_path(t.path()).unwrap();
        let layout = SessionLayout::new(
            &state,
            &SessionId::new("01HZY3K4M5N6P7Q8R9S0T1V2W3").unwrap(),
        );
        assert!(matches!(
            load_manifest(&layout),
            Err(CredentialError::NoManifest(_))
        ));
        std::fs::create_dir_all(layout.root().as_path()).unwrap();
        let m = CredentialManifest {
            agent_dir: AbsPath::new("/home/alice/.pi/agent").unwrap(),
            api_key_providers: vec!["gateway".into()],
            oauth_providers: vec!["sub".into()],
            oauth_helper: Some(
                AbsPath::new("/nix/store/aaaa-controlroom-pi/bin/controlroom-pi-oauth").unwrap(),
            ),
            stored_keys: None,
        };
        std::fs::write(
            layout.credential_manifest().as_path(),
            serde_json::to_vec(&m).unwrap(),
        )
        .unwrap();
        assert_eq!(load_manifest(&layout).unwrap(), m);
    }

    #[test]
    fn oauth_wins_over_an_api_key() {
        let t = tempfile::tempdir().unwrap();
        let m = CredentialManifest {
            agent_dir: AbsPath::from_path(t.path()).unwrap(),
            api_key_providers: vec!["sub".into()],
            oauth_providers: vec!["sub".into()],
            oauth_helper: None,
            stored_keys: None,
        };
        let sub = Name::new("sub").unwrap();
        assert!(matches!(
            resolve_credential(&m, &sub, 600),
            Err(CredentialError::NoOAuthHelper(_))
        ));
    }

    #[test]
    fn the_user_config_wins_over_stored_keys() {
        let t = tempfile::tempdir().unwrap();
        let dir = AbsPath::from_path(t.path()).unwrap();
        std::fs::write(
            t.path().join("models.json"),
            r#"{"providers": {"gateway": {"apiKey": "from-models"}}}"#,
        )
        .unwrap();
        let mut store = KeyStore::default();
        store.insert("gateway", "from-store");
        store.insert("anthropic", "stored-key");
        store.save(&dir.join("keys.json")).unwrap();
        let m = CredentialManifest {
            agent_dir: dir.clone(),
            api_key_providers: vec!["gateway".into()],
            oauth_providers: Vec::new(),
            oauth_helper: None,
            stored_keys: Some(StoredKeys {
                path: dir.join("keys.json"),
                providers: vec!["anthropic".into()],
            }),
        };
        let get = |p: &str| resolve_credential(&m, &Name::new(p).unwrap(), 600);
        assert_eq!(get("gateway").unwrap().expose(), "from-models");
        assert_eq!(get("anthropic").unwrap().expose(), "stored-key");
        assert!(matches!(
            get("openai"),
            Err(CredentialError::UnknownProvider(_))
        ));
    }

    #[test]
    fn older_manifests_have_no_oauth() {
        let m: CredentialManifest = serde_json::from_str(
            r#"{"agent_dir": "/home/alice/.pi/agent", "api_key_providers": ["gateway"]}"#,
        )
        .unwrap();
        assert!(m.oauth_providers.is_empty() && m.oauth_helper.is_none());
    }
}
