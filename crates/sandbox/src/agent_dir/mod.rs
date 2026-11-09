//! The per-session pi agent directory (spec §11.1).
//!
//! [`build_session_dirs`] creates `agent/`, `home/` and `tmp/` under the session root and
//! fills `agent/` from the user's real pi agent dir, which is only ever read:
//!
//! - `settings.json`: the user's settings filtered to the keep-list, `"transport": "sse"`;
//! - `models.json`: the user's with every key and secret header replaced;
//! - `auth.json`: fake tokens only (placeholders, synthetic unsigned JWTs);
//! - `npm` → the pinned extension bundle;
//! - `sessions/`: pi's session store, preserved across rebuilds.
//!
//! Providers managed in Settings › Models ([`ManagedProviders`]) are merged in: stored API keys
//! get placeholder `auth.json` entries, custom gateways `models.json` entries, the defaults of
//! new sessions override `settings.json`. The user's own config wins on a name clash.
//!
//! It also writes `credentials.json` next to (not inside) `agent/`, telling
//! `controlroomd credential` where the real keys live. Rebuilding (on restart) regenerates
//! the files and keeps pi's sessions, `home/` and `tmp/`.

mod auth;
mod models;
mod settings;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;

use serde_json::{Map, Value};

pub use auth::{FAR_FUTURE_MS, FakeAuth, fake_auth, unsigned_jwt};
pub use models::sanitize_models;
pub use settings::session_settings;

use crate::credential::{CredentialManifest, StoredKeys, models_api_key_providers};
use crate::fsutil::{PathIoError, at, copy_tree, create_private_dir, write_atomic};
use crate::layout::SessionLayout;
use crate::paths::AbsPath;
use crate::profile::{NonoProfile, ProfileError, check_profile_json};

/// The value standing in for every real key or token inside the sandbox.
pub const PLACEHOLDER: &str = "controlroom-placeholder";

/// Files in the agent dir that are always generated, never copied.
const GENERATED: [&str; 3] = ["settings.json", "models.json", "auth.json"];

/// Inputs of [`build_session_dirs`].
#[derive(Debug, Clone, Default)]
pub struct AgentDirSpec {
    /// The user's real pi agent dir (read only).
    pub user_agent_dir: Option<AbsPath>,
    /// Resource sources (packages, extensions, skills, prompts, themes) to keep.
    pub keep: Vec<String>,
    /// Target of the `agent/npm` symlink (the pinned extension bundle).
    pub npm: Option<AbsPath>,
    /// Extra files or directories copied from the user's agent dir, relative to it
    /// (e.g. `AGENTS.md`, an extension's config file).
    pub copy: Vec<String>,
    /// Per OAuth provider, the JWT claims the synthetic token carries.
    pub oauth_claims: BTreeMap<String, Vec<String>>,
    /// A directory copied into `tmp/` without overwriting (a pre-warmed jiti cache).
    pub tmp_seed: Option<AbsPath>,
    /// The OAuth helper `controlroomd credential` runs for OAuth providers.
    pub oauth_helper: Option<AbsPath>,
    /// Providers managed outside the user's agent dir (Settings › Models).
    pub managed: ManagedProviders,
}

/// Providers managed outside the user's agent dir, merged into the session's config.
#[derive(Debug, Clone, Default)]
pub struct ManagedProviders {
    /// Providers whose API key is in [`Self::key_store`].
    pub key_providers: Vec<String>,
    /// The key store `controlroomd credential` reads their keys from.
    pub key_store: Option<AbsPath>,
    /// `models.json` provider entries (custom gateways), without real keys.
    pub models: Map<String, Value>,
    /// `settings.json` values that override the user's (the defaults of new sessions).
    pub settings: Map<String, Value>,
}

/// What the build found: which providers have which credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltAgentDir {
    /// Providers configured with API keys in the user's `auth.json` or `models.json`.
    pub api_key_providers: Vec<String>,
    /// Providers configured with OAuth tokens.
    pub oauth_providers: Vec<String>,
    /// Providers whose key comes from the key store.
    pub stored_key_providers: Vec<String>,
}

/// Errors building the session directories.
#[derive(Debug, thiserror::Error)]
pub enum AgentDirError {
    /// Filesystem error.
    #[error(transparent)]
    Io(#[from] PathIoError),
    /// A user config file is not a JSON object.
    #[error("{0}: expected a JSON object")]
    NotAnObject(String),
    /// A user config file is not valid JSON.
    #[error("{path}: {source}")]
    Json {
        /// The file.
        path: String,
        /// The parse error.
        #[source]
        source: serde_json::Error,
    },
    /// A copy entry is absolute, escapes the agent dir, or names a generated file.
    #[error("invalid copy entry {0:?}")]
    InvalidCopy(String),
    /// The generated profile failed validation.
    #[error(transparent)]
    Profile(#[from] ProfileError),
}

/// Creates or refreshes the session directories for `layout`.
pub fn build_session_dirs(
    layout: &SessionLayout,
    spec: &AgentDirSpec,
) -> Result<BuiltAgentDir, AgentDirError> {
    create_private_dir(layout.root().as_path())?;
    for dir in layout.sandbox_dirs() {
        create_private_dir(dir.as_path())?;
    }
    let agent = layout.agent();
    create_private_dir(agent.join("sessions").as_path())?;
    let built = build_agent_config(&agent, spec)?;

    let user = spec.user_agent_dir.as_ref();
    if let Some(target) = &spec.npm {
        replace_symlink(target, &agent.join("npm"))?;
    }
    if let Some(dir) = user {
        for rel in &spec.copy {
            check_copy_entry(rel)?;
            let from = dir.join(rel);
            if from.as_path().exists() {
                copy_tree(from.as_path(), agent.join(rel).as_path(), true)?;
            }
        }
    }
    if let Some(seed) = &spec.tmp_seed {
        copy_tree(seed.as_path(), layout.tmp().as_path(), false)?;
    }

    if let Some(dir) = user {
        let manifest = CredentialManifest {
            agent_dir: dir.clone(),
            api_key_providers: built.api_key_providers.clone(),
            oauth_providers: built.oauth_providers.clone(),
            oauth_helper: spec.oauth_helper.clone(),
            stored_keys: spec.managed.key_store.clone().map(|path| StoredKeys {
                path,
                providers: built.stored_key_providers.clone(),
            }),
        };
        let bytes = serde_json::to_vec_pretty(&manifest).expect("manifest serializes");
        write_atomic(layout.credential_manifest().as_path(), &bytes, 0o600)?;
    }
    Ok(built)
}

/// Writes pi's configuration into `agent` (created if missing): `settings.json`, `models.json`
/// and `auth.json` from the user's agent dir merged with [`AgentDirSpec::managed`], with fake
/// credentials only. Sessions get it through [`build_session_dirs`]; the agent daemon also
/// writes it elsewhere to ask pi which models these providers offer.
pub fn build_agent_config(
    agent: &AbsPath,
    spec: &AgentDirSpec,
) -> Result<BuiltAgentDir, AgentDirError> {
    create_private_dir(agent.as_path())?;
    let read = |name: &str| match &spec.user_agent_dir {
        Some(dir) => read_json_object(&dir.join(name)),
        None => Ok(None),
    };
    let managed = &spec.managed;

    let mut settings = session_settings(&read("settings.json")?.unwrap_or_default(), &spec.keep);
    settings.extend(managed.settings.clone());
    write_json(&agent.join("settings.json"), &settings)?;

    let models_path = agent.join("models.json");
    let user_models = read("models.json")?;
    let mut api_key_providers = user_models
        .as_ref()
        .map(models_api_key_providers)
        .unwrap_or_default();
    match merge_models(user_models, &managed.models) {
        Some(models) => {
            let mut models = Value::Object(models);
            sanitize_models(&mut models);
            write_json_value(&models_path, &models)?;
        }
        None => remove_if_exists(&models_path)?,
    }

    let mut fake = fake_auth(&read("auth.json")?.unwrap_or_default(), &spec.oauth_claims);
    api_key_providers.extend(fake.api_key_providers.iter().cloned());
    api_key_providers.sort();
    api_key_providers.dedup();
    // Stored keys, unless the user's config already has a credential for the provider.
    let mut stored_key_providers = Vec::new();
    for provider in &managed.key_providers {
        let configured =
            api_key_providers.contains(provider) || fake.oauth_providers.contains(provider);
        if !configured {
            fake.auth.insert(provider.clone(), placeholder_key());
            stored_key_providers.push(provider.clone());
        }
    }
    write_json(&agent.join("auth.json"), &fake.auth)?;
    Ok(BuiltAgentDir {
        api_key_providers,
        oauth_providers: fake.oauth_providers,
        stored_key_providers,
    })
}

/// The user's `models.json` with the managed providers added (the user's win on a name
/// clash); `None` when there is neither.
fn merge_models(
    user: Option<Map<String, Value>>,
    managed: &Map<String, Value>,
) -> Option<Map<String, Value>> {
    if managed.is_empty() {
        return user;
    }
    let mut models = user.unwrap_or_default();
    let providers = models
        .entry("providers")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(providers) = providers {
        for (name, provider) in managed {
            providers
                .entry(name.clone())
                .or_insert_with(|| provider.clone());
        }
    }
    Some(models)
}

/// An `auth.json` API-key entry holding the placeholder.
fn placeholder_key() -> Value {
    serde_json::json!({"type": "api_key", "key": PLACEHOLDER})
}

/// Checks `profile` with [`check_profile_json`] and writes it to `profile.json`.
pub fn write_profile(layout: &SessionLayout, profile: &NonoProfile) -> Result<(), AgentDirError> {
    check_profile_json(&profile.to_value())?;
    create_private_dir(layout.root().as_path())?;
    write_atomic(
        layout.profile().as_path(),
        profile.to_json_pretty().as_bytes(),
        0o600,
    )?;
    Ok(())
}

fn check_copy_entry(rel: &str) -> Result<(), AgentDirError> {
    let ok = !rel.is_empty()
        && !rel.starts_with('/')
        && rel
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
        && !GENERATED.contains(&rel)
        && rel.split('/').next() != Some("sessions")
        && rel != "npm";
    if ok {
        Ok(())
    } else {
        Err(AgentDirError::InvalidCopy(rel.to_owned()))
    }
}

fn read_json_object(path: &AbsPath) -> Result<Option<Map<String, Value>>, AgentDirError> {
    let bytes = match fs::read(path.as_path()) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(path, e)),
    };
    match serde_json::from_slice(&bytes) {
        Ok(Value::Object(m)) => Ok(Some(m)),
        Ok(_) => Err(AgentDirError::NotAnObject(path.to_string())),
        Err(source) => Err(AgentDirError::Json {
            path: path.to_string(),
            source,
        }),
    }
}

fn write_json(path: &AbsPath, map: &Map<String, Value>) -> Result<(), AgentDirError> {
    let mut bytes = serde_json::to_vec_pretty(map).expect("JSON map serializes");
    bytes.push(b'\n');
    Ok(write_atomic(path.as_path(), &bytes, 0o600)?)
}

fn write_json_value(path: &AbsPath, value: &Value) -> Result<(), AgentDirError> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("JSON value serializes");
    bytes.push(b'\n');
    Ok(write_atomic(path.as_path(), &bytes, 0o600)?)
}

fn remove_if_exists(path: &AbsPath) -> Result<(), AgentDirError> {
    match fs::remove_file(path.as_path()) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_error(path, e)),
        _ => Ok(()),
    }
}

fn io_error(path: &AbsPath, source: io::Error) -> AgentDirError {
    AgentDirError::Io(PathIoError {
        path: path.as_path().to_owned(),
        source,
    })
}

/// Points `link` at `target`, replacing an existing symlink atomically.
fn replace_symlink(target: &AbsPath, link: &AbsPath) -> Result<(), AgentDirError> {
    let tmp = link.parent().expect("link has a parent").join(".npm.tmp");
    let _ = fs::remove_file(tmp.as_path());
    at(tmp.as_path(), symlink(target.as_path(), tmp.as_path()))?;
    at(link.as_path(), fs::rename(tmp.as_path(), link.as_path()))?;
    Ok(())
}

#[cfg(test)]
mod tests;
