//! Host-side credential lookup for `controlroomd credential <session> <provider>` (§11.3).
//!
//! nono's `credential_capture` runs the helper unsandboxed and injects its stdout into
//! requests on the provider's route. The helper reads the session's [`CredentialManifest`]
//! (written next to, not inside, the sandboxed agent dir) and resolves the provider's API key
//! from the user's real pi config exactly as pi would: `auth.json` first, then
//! `models.json`'s `apiKey`, each value being a literal, `$ENV`/`${ENV}` template or
//! `!command`. Keys never travel through argv. OAuth refresh is out of scope here.

use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::layout::SessionLayout;
use crate::names::Name;
use crate::paths::AbsPath;

/// How long a `!command` key may take (pi uses the same limit).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Written by the agent-dir builder; tells the helper where the real config lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialManifest {
    /// The user's real pi agent dir.
    pub agent_dir: AbsPath,
    /// Providers the helper may resolve.
    pub api_key_providers: Vec<String>,
}

/// A resolved key. `Debug` does not print it.
pub struct Secret(String);

impl Secret {
    /// The key. Write it only to the helper's stdout.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Why no key was produced. Messages never contain key material.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// The session has no manifest (not built, or no user agent dir).
    #[error("no credential manifest for this session: {0}")]
    NoManifest(String),
    /// The provider is not an API-key provider of this session.
    #[error("provider {0} has no API key in this session")]
    UnknownProvider(String),
    /// The configured value could not be resolved (missing env var, failing command).
    #[error("the API key of provider {0} could not be resolved")]
    Unresolved(String),
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

/// Resolves `provider`'s API key from the user's real config.
pub fn resolve_api_key(
    manifest: &CredentialManifest,
    provider: &Name,
) -> Result<Secret, CredentialError> {
    let name = provider.as_str();
    if !manifest.api_key_providers.iter().any(|p| p == name) {
        return Err(CredentialError::UnknownProvider(name.to_owned()));
    }
    let auth = read_object(&manifest.agent_dir.join("auth.json"))?;
    let from_auth = auth
        .get(name)
        .filter(|e| e.get("type").and_then(Value::as_str) == Some("api_key"))
        .and_then(|e| {
            let key = e.get("key")?.as_str()?;
            let env = string_map(e.get("env"));
            Some((key.to_owned(), env))
        });
    let (config, env) = match from_auth {
        Some(found) => found,
        None => {
            let models = read_object(&manifest.agent_dir.join("models.json"))?;
            let key = models
                .get("providers")
                .and_then(|p| p.get(name))
                .and_then(|p| p.get("apiKey"))
                .and_then(Value::as_str)
                .ok_or_else(|| CredentialError::UnknownProvider(name.to_owned()))?;
            (key.to_owned(), HashMap::new())
        }
    };
    resolve_config_value(&config, &|var| {
        env.get(var)
            .cloned()
            .or_else(|| std::env::var(var).ok())
            .filter(|v| !v.is_empty())
    })
    .map(Secret)
    .ok_or_else(|| CredentialError::Unresolved(name.to_owned()))
}

/// Providers with an `apiKey` in a `models.json` document.
pub fn models_api_key_providers(models: &Map<String, Value>) -> Vec<String> {
    models
        .get("providers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, p)| p.get("apiKey").is_some_and(Value::is_string))
        .map(|(name, _)| name.clone())
        .collect()
}

fn read_object(path: &AbsPath) -> Result<Map<String, Value>, CredentialError> {
    let bytes = match std::fs::read(path.as_path()) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => {
            return Err(CredentialError::Config {
                path: path.to_string(),
                problem: e.to_string(),
            });
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(Value::Object(m)) => Ok(m),
        // Do not echo parse errors: they can quote file content.
        _ => Err(CredentialError::Config {
            path: path.to_string(),
            problem: "not a JSON object".to_owned(),
        }),
    }
}

fn string_map(v: Option<&Value>) -> HashMap<String, String> {
    v.and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
        .collect()
}

/// Resolves a pi config value: `!command` runs through `/bin/sh` (stdout, trimmed);
/// otherwise `$NAME`/`${NAME}` are interpolated with `lookup` (`$$` → `$`, `$!` → `!`) and
/// any missing variable makes the whole value unresolved. Empty results are `None`.
pub fn resolve_config_value(
    config: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    if let Some(command) = config.strip_prefix('!') {
        return run_command(command);
    }
    let mut out = String::new();
    let mut rest = config;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.chars().next() {
            Some(c @ ('$' | '!')) => {
                out.push(c);
                rest = &after[1..];
            }
            Some('{') => match after.find('}') {
                Some(end) if is_env_name(&after[1..end]) => {
                    out.push_str(&lookup(&after[1..end])?);
                    rest = &after[end + 1..];
                }
                Some(end) => {
                    out.push_str(&rest[i..i + 1 + end + 1]);
                    rest = &after[end + 1..];
                }
                None => {
                    out.push('$');
                    rest = after;
                }
            },
            _ => {
                let len = env_name_prefix_len(after);
                if len == 0 {
                    out.push('$');
                    rest = after;
                } else {
                    out.push_str(&lookup(&after[..len])?);
                    rest = &after[len..];
                }
            }
        }
    }
    out.push_str(rest);
    (!out.is_empty()).then_some(out)
}

fn is_env_name(s: &str) -> bool {
    env_name_prefix_len(s) == s.len() && !s.is_empty()
}

fn env_name_prefix_len(s: &str) -> usize {
    let mut len = 0;
    for (i, b) in s.bytes().enumerate() {
        let ok = b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit());
        if !ok {
            break;
        }
        len = i + 1;
    }
    len
}

/// Runs `command` with `/bin/sh -c`, stdin closed, stderr discarded, killed after
/// [`COMMAND_TIMEOUT`]. Returns trimmed stdout on success.
fn run_command(command: &str) -> Option<String> {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        stdout.read_to_string(&mut buf).map(|_| buf)
    });
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok()?.ok()?;
    let out = out.trim();
    (status.success() && !out.is_empty()).then(|| out.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::SessionId;

    fn lookup(var: &str) -> Option<String> {
        match var {
            "KEY" => Some("k1".into()),
            "B2" => Some("b".into()),
            _ => None,
        }
    }

    #[test]
    fn config_value_templates() {
        let r = |s| resolve_config_value(s, &lookup);
        assert_eq!(r("literal").as_deref(), Some("literal"));
        assert_eq!(r("$KEY").as_deref(), Some("k1"));
        assert_eq!(r("${KEY}-x").as_deref(), Some("k1-x"));
        assert_eq!(r("a$B2.c").as_deref(), Some("ab.c"));
        assert_eq!(r("$$KEY").as_deref(), Some("$KEY"));
        assert_eq!(r("$!x").as_deref(), Some("!x"));
        assert_eq!(r("${not valid}").as_deref(), Some("${not valid}"));
        assert_eq!(r("cost $5").as_deref(), Some("cost $5"));
        assert_eq!(r("$MISSING"), None);
        assert_eq!(r(""), None);
    }

    #[test]
    fn config_value_commands() {
        let r = |s| resolve_config_value(s, &lookup);
        assert_eq!(r("!printf ' from-cmd \\n'").as_deref(), Some("from-cmd"));
        assert_eq!(r("!exit 3"), None);
        assert_eq!(r("!true"), None);
    }

    fn setup(auth: Value, models: Value) -> (tempfile::TempDir, CredentialManifest) {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("auth.json"), auth.to_string()).unwrap();
        std::fs::write(t.path().join("models.json"), models.to_string()).unwrap();
        let mut providers = vec!["gateway".to_owned()];
        providers.extend(models_api_key_providers(models.as_object().unwrap()));
        let m = CredentialManifest {
            agent_dir: AbsPath::from_path(t.path()).unwrap(),
            api_key_providers: providers,
        };
        (t, m)
    }

    #[test]
    fn resolves_auth_then_models() {
        let (_t, m) = setup(
            serde_json::json!({
                "gateway": {"type": "api_key", "key": "sk-${PART}", "env": {"PART": "abc"}},
                "sub": {"type": "oauth", "access": "x"}
            }),
            serde_json::json!({"providers": {"local": {"apiKey": "!printf local-key"}}}),
        );
        let name = |s| Name::new(s).unwrap();
        assert_eq!(
            resolve_api_key(&m, &name("gateway")).unwrap().expose(),
            "sk-abc"
        );
        assert_eq!(
            resolve_api_key(&m, &name("local")).unwrap().expose(),
            "local-key"
        );
        assert!(matches!(
            resolve_api_key(&m, &name("sub")),
            Err(CredentialError::UnknownProvider(_))
        ));
        let secret = resolve_api_key(&m, &name("gateway")).unwrap();
        assert!(!format!("{secret:?}").contains("abc"));
    }

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
        };
        std::fs::write(
            layout.credential_manifest().as_path(),
            serde_json::to_vec(&m).unwrap(),
        )
        .unwrap();
        assert_eq!(load_manifest(&layout).unwrap(), m);
    }
}
