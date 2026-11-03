//! Configuration: a TOML file, overridden by flags and environment variables, resolved and
//! validated into [`Settings`].
//!
//! The startup refusals live here, so they are testable without binding sockets:
//! - no master key → refuse (it is never generated next to the database);
//! - dev-token mode anywhere but a loopback TCP address, or with a non-loopback public URL →
//!   refuse (so a non-loopback listener always needs OIDC);
//! - OIDC without an https bare-origin `public_url`, issuer, client, secret or allow-list →
//!   refuse.
//!
//! Secrets come only from files. Relative secret paths name systemd credentials
//! (`$CREDENTIALS_DIRECTORY/<name>`, from `LoadCredential=`).
//!
//! A deployment under systemd (`StateDirectory=`, `LoadCredential=master-key:…`,
//! `oidc-client-secret:…`, `vapid-key:…`):
//!
//! ```toml
//! listen = "127.0.0.1:8080"            # or "unix:/run/controlroom/web.sock"
//! public_url = "https://controlroom.example.com"
//!
//! [auth.oidc]
//! issuer = "https://id.example.com"
//! client_id = "controlroom"           # or client_id_file = "oidc-client-id"
//! client_secret_file = "oidc-client-secret"
//! allowed_groups = ["controlroom-users"]
//!
//! [push]
//! vapid_key_file = "vapid-key"
//! ```
//!
//! Development: `controlroom-web --state-dir /var/tmp/cr --master-key-file /var/tmp/cr-key
//! --auth-mode dev-token` prints the sign-in URL.

mod file;
mod listen;

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use url::Url;

pub use file::{AuthConfig, AuthModeArg, Cli, Config, OidcConfig, PushConfig};
pub use listen::Listen;

/// The default listen address.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8080";
/// The default sliding login lifetime.
pub const DEFAULT_SESSION_TTL_DAYS: u32 = 30;
/// The master key's credential name when no file is configured.
pub const MASTER_KEY_CREDENTIAL: &str = "master-key";

/// The bundled daemons of this build: the `controlroom-web` Nix package sets
/// `CONTROLROOM_BUNDLED_DAEMONS_DIR` at compile time to its own `share/controlroom/daemons`.
pub const BUNDLED_DAEMONS_DIR: Option<&str> = option_env!("CONTROLROOM_BUNDLED_DAEMONS_DIR");

/// The process environment that configuration depends on (systemd conventions).
#[derive(Debug, Clone, Default)]
pub struct Env {
    /// `$CREDENTIALS_DIRECTORY` (`LoadCredential=`).
    pub credentials_directory: Option<PathBuf>,
    /// `$STATE_DIRECTORY` (`StateDirectory=`).
    pub state_directory: Option<PathBuf>,
}

impl Env {
    /// Reads the process environment.
    pub fn from_process() -> Self {
        let dir = |name: &str| {
            let value = std::env::var_os(name)?;
            let value = value.to_str()?;
            // systemd may list several colon-separated directories; the first is ours.
            let first = value.split(':').next().filter(|s| !s.is_empty())?;
            Some(PathBuf::from(first))
        };
        Self {
            credentials_directory: dir("CREDENTIALS_DIRECTORY"),
            state_directory: dir("STATE_DIRECTORY"),
        }
    }

    /// A configured secret path: relative paths are credentials.
    fn secret_path(&self, path: &Path) -> Result<PathBuf, ConfigError> {
        if path.is_absolute() {
            return Ok(path.to_owned());
        }
        match &self.credentials_directory {
            Some(dir) => Ok(dir.join(path)),
            None => Err(ConfigError::Invalid(format!(
                "{} is a relative secret path, which names a systemd credential, but \
                 $CREDENTIALS_DIRECTORY is not set; use an absolute path",
                path.display()
            ))),
        }
    }
}

/// Validated configuration.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Where to listen.
    pub listen: Listen,
    /// The origin browsers use. `None` only in dev-token mode, where it is the bound loopback
    /// address.
    pub public_url: Option<Url>,
    /// The state directory.
    pub state_dir: PathBuf,
    /// The master key file.
    pub master_key_file: PathBuf,
    /// The bundled daemons; `None`: machines can only use an installed daemon.
    pub bundled_daemons_dir: Option<PathBuf>,
    /// How long a login lasts without use.
    pub session_ttl: Duration,
    /// Authentication.
    pub auth: AuthSettings,
    /// Web Push; `None` when off.
    pub push: Option<PushSettings>,
}

/// The auth mode with its settings.
#[derive(Debug, Clone)]
pub enum AuthSettings {
    /// OIDC.
    Oidc(OidcSettings),
    /// The development token.
    DevToken,
}

/// OIDC settings.
#[derive(Debug, Clone)]
pub struct OidcSettings {
    /// The issuer URL.
    pub issuer: String,
    /// The client id.
    pub client_id: ClientIdSource,
    /// The client secret file.
    pub client_secret_file: PathBuf,
    /// Subjects that may sign in.
    pub allowed_subjects: Vec<String>,
    /// Groups whose members may sign in.
    pub allowed_groups: Vec<String>,
    /// Scopes requested besides `openid`.
    pub scopes: Vec<String>,
}

/// Where the OIDC client id comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientIdSource {
    /// Configured inline.
    Value(String),
    /// Read from a file (a credential) at start.
    File(PathBuf),
}

/// Web Push settings.
#[derive(Debug, Clone)]
pub struct PushSettings {
    /// The VAPID private key file.
    pub vapid_key_file: PathBuf,
    /// The VAPID contact; `None`: the public URL.
    pub subject: Option<String>,
}

/// Why the configuration is refused.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// No master key.
    #[error(
        "refusing to start: the master key is required. It encrypts the SSH keys in the \
         database and is never generated next to it. Pass it with systemd \
         LoadCredential={MASTER_KEY_CREDENTIAL}:<path>, master_key_file, --master-key-file or \
         CONTROLROOM_WEB_MASTER_KEY_FILE"
    )]
    MissingMasterKey,
    /// Dev-token mode outside loopback.
    #[error(
        "refusing to start: {what} is not loopback and the auth mode is not oidc. dev-token \
         mode only serves a loopback TCP address (e.g. 127.0.0.1:8080); configure [auth.oidc] \
         for anything else"
    )]
    NotLoopbackWithoutOidc {
        /// The offending address or URL.
        what: String,
    },
    /// Anything else.
    #[error("invalid configuration: {0}")]
    Invalid(String),
    /// The file couldn't be read.
    #[error("reading {path}: {source}")]
    Read {
        /// The file.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
    /// The file isn't valid TOML for this schema.
    #[error("parsing {path}: {source}")]
    Parse {
        /// The file.
        path: PathBuf,
        /// Why.
        source: Box<toml::de::Error>,
    },
}

impl Config {
    /// Reads a TOML file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        })
    }

    /// Validates and resolves the configuration.
    pub fn resolve(self, env: &Env) -> Result<Settings, ConfigError> {
        let master_key_file = match &self.master_key_file {
            Some(path) => env.secret_path(path)?,
            None => env
                .credentials_directory
                .as_ref()
                .map(|dir| dir.join(MASTER_KEY_CREDENTIAL))
                .ok_or(ConfigError::MissingMasterKey)?,
        };
        let listen = Listen::parse(self.listen.as_deref().unwrap_or(DEFAULT_LISTEN))?;
        let public_url = self
            .public_url
            .as_deref()
            .map(parse_public_url)
            .transpose()?;
        let mode = self.auth.mode.unwrap_or(match self.auth.oidc {
            Some(_) => AuthModeArg::Oidc,
            None => AuthModeArg::DevToken,
        });
        let auth = match mode {
            AuthModeArg::DevToken => {
                if !listen.is_loopback_tcp() {
                    return Err(ConfigError::NotLoopbackWithoutOidc {
                        what: listen.to_string(),
                    });
                }
                if let Some(url) = public_url.as_ref().filter(|u| !is_loopback_host(u)) {
                    return Err(ConfigError::NotLoopbackWithoutOidc {
                        what: url.to_string(),
                    });
                }
                AuthSettings::DevToken
            }
            AuthModeArg::Oidc => {
                let url = public_url.as_ref().ok_or_else(|| {
                    ConfigError::Invalid(
                        "public_url is required with oidc: it defines the redirect URI, the \
                         Host allowlist and the expected Origin"
                            .into(),
                    )
                })?;
                if url.scheme() != "https" {
                    return Err(ConfigError::Invalid(
                        "public_url must be https with oidc (login cookies are Secure)".into(),
                    ));
                }
                let oidc = self.auth.oidc.ok_or_else(|| {
                    ConfigError::Invalid("auth mode oidc needs an [auth.oidc] section".into())
                })?;
                AuthSettings::Oidc(resolve_oidc(oidc, env)?)
            }
        };
        let state_dir = self
            .state_dir
            .or_else(|| env.state_directory.clone())
            .ok_or_else(|| {
                ConfigError::Invalid(
                    "state_dir is required (or run under systemd with StateDirectory=)".into(),
                )
            })?;
        let ttl_days = self.session_ttl_days.unwrap_or(DEFAULT_SESSION_TTL_DAYS);
        if ttl_days == 0 {
            return Err(ConfigError::Invalid("session_ttl_days must be > 0".into()));
        }
        let push = match self.push.vapid_key_file {
            Some(path) => Some(PushSettings {
                vapid_key_file: env.secret_path(&path)?,
                subject: self.push.subject,
            }),
            None => None,
        };
        Ok(Settings {
            listen,
            public_url,
            state_dir,
            master_key_file,
            bundled_daemons_dir: self
                .bundled_daemons_dir
                .or_else(|| BUNDLED_DAEMONS_DIR.map(PathBuf::from)),
            session_ttl: Duration::from_secs(u64::from(ttl_days) * 24 * 60 * 60),
            auth,
            push,
        })
    }
}

fn resolve_oidc(oidc: OidcConfig, env: &Env) -> Result<OidcSettings, ConfigError> {
    let missing = |field: &str| ConfigError::Invalid(format!("auth.oidc.{field} is required"));
    let issuer = oidc.issuer.ok_or_else(|| missing("issuer"))?;
    Url::parse(&issuer).map_err(|e| ConfigError::Invalid(format!("auth.oidc.issuer: {e}")))?;
    let client_id = match (oidc.client_id, oidc.client_id_file) {
        (Some(id), None) => ClientIdSource::Value(id),
        (None, Some(path)) => ClientIdSource::File(env.secret_path(&path)?),
        (None, None) => return Err(missing("client_id (or client_id_file)")),
        (Some(_), Some(_)) => {
            return Err(ConfigError::Invalid(
                "auth.oidc: set client_id or client_id_file, not both".into(),
            ));
        }
    };
    let client_secret_file = env.secret_path(
        &oidc
            .client_secret_file
            .ok_or_else(|| missing("client_secret_file"))?,
    )?;
    if oidc.allowed_subjects.is_empty() && oidc.allowed_groups.is_empty() {
        return Err(ConfigError::Invalid(
            "auth.oidc needs allowed_subjects and/or allowed_groups; nobody could sign in".into(),
        ));
    }
    let mut scopes = vec!["profile".to_owned(), "email".to_owned()];
    if !oidc.allowed_groups.is_empty() {
        scopes.push("groups".to_owned());
    }
    for scope in oidc.extra_scopes {
        if scope != "openid" && !scopes.contains(&scope) {
            scopes.push(scope);
        }
    }
    Ok(OidcSettings {
        issuer,
        client_id,
        client_secret_file,
        allowed_subjects: oidc.allowed_subjects,
        allowed_groups: oidc.allowed_groups,
        scopes,
    })
}

/// `public_url` must be a bare origin: the app is served at `/` (the `__Host-` cookie prefix
/// requires `Path=/`, and the redirect URI is `<origin>/auth/callback`).
fn parse_public_url(value: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(value)
        .map_err(|e| ConfigError::Invalid(format!("public_url {value:?}: {e}")))?;
    let bare = matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    if !bare {
        return Err(ConfigError::Invalid(format!(
            "public_url must be a bare origin like https://controlroom.example.com, got {value:?}"
        )));
    }
    Ok(url)
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(toml: &str) -> Result<Settings, ConfigError> {
        Config::from_toml(toml).unwrap().resolve(&Env::default())
    }

    const OIDC: &str = r#"
        listen = "127.0.0.1:8080"
        public_url = "https://controlroom.example.com"
        state_dir = "/var/lib/controlroom"
        master_key_file = "/run/keys/master-key"
        [auth.oidc]
        issuer = "https://id.example.com"
        client_id = "controlroom"
        client_secret_file = "/run/keys/oidc"
        allowed_subjects = ["alice"]
    "#;

    const DEV: &str = "state_dir = \"/tmp/cr\"\nmaster_key_file = \"/tmp/mk\"\n";

    #[test]
    fn oidc_resolves() {
        let s = resolve(OIDC).unwrap();
        let AuthSettings::Oidc(oidc) = &s.auth else {
            panic!("oidc expected")
        };
        assert_eq!(oidc.scopes, ["profile", "email"]);
        assert_eq!(s.session_ttl, Duration::from_secs(30 * 86_400));
        assert!(s.push.is_none());
        let groups = OIDC.replace(
            "allowed_subjects = [\"alice\"]",
            "allowed_groups = [\"staff\"]\nextra_scopes = [\"openid\", \"offline_access\"]",
        );
        let AuthSettings::Oidc(oidc) = resolve(&groups).unwrap().auth else {
            panic!("oidc expected")
        };
        assert_eq!(
            oidc.scopes,
            ["profile", "email", "groups", "offline_access"]
        );
    }

    #[test]
    fn the_master_key_is_required() {
        let err = resolve("state_dir = \"/tmp/cr\"").unwrap_err();
        assert!(matches!(err, ConfigError::MissingMasterKey), "{err}");
        assert!(err.to_string().contains("master key is required"));
    }

    #[test]
    fn dev_token_is_loopback_only() {
        assert!(matches!(resolve(DEV).unwrap().auth, AuthSettings::DevToken));
        for listen in [
            "0.0.0.0:8080",
            "192.168.1.2:80",
            "[::]:80",
            "unix:/run/cr.sock",
        ] {
            let err = resolve(&format!("{DEV}listen = \"{listen}\"")).unwrap_err();
            assert!(
                matches!(err, ConfigError::NotLoopbackWithoutOidc { .. }),
                "{listen}: {err}"
            );
        }
        let proxied = format!("{DEV}public_url = \"https://controlroom.example.com\"");
        assert!(matches!(
            resolve(&proxied).unwrap_err(),
            ConfigError::NotLoopbackWithoutOidc { .. }
        ));
        let local = format!("{DEV}public_url = \"http://localhost:8080\"");
        assert!(resolve(&local).is_ok());
    }

    #[test]
    fn oidc_needs_an_https_origin_and_an_allow_list() {
        let cases = [
            OIDC.replace("public_url = \"https://controlroom.example.com\"", ""),
            OIDC.replace(
                "https://controlroom.example.com",
                "http://controlroom.example.com",
            ),
            OIDC.replace("https://controlroom.example.com", "https://example.com/cr"),
            OIDC.replace("allowed_subjects = [\"alice\"]", ""),
            OIDC.replace("client_secret_file = \"/run/keys/oidc\"", ""),
        ];
        for case in cases {
            assert!(resolve(&case).is_err(), "{case}");
        }
        // A non-loopback listener is fine with OIDC.
        assert!(resolve(&OIDC.replace("127.0.0.1:8080", "0.0.0.0:8080")).is_ok());
    }

    #[test]
    fn relative_secrets_are_credentials() {
        let env = Env {
            credentials_directory: Some("/run/credentials/controlroom.service".into()),
            state_directory: Some("/var/lib/controlroom".into()),
        };
        let s = Config::from_toml("[push]\nvapid_key_file = \"vapid\"")
            .unwrap()
            .resolve(&env)
            .unwrap();
        assert_eq!(
            s.master_key_file,
            Path::new("/run/credentials/controlroom.service/master-key")
        );
        assert_eq!(s.state_dir, Path::new("/var/lib/controlroom"));
        assert_eq!(
            s.push.unwrap().vapid_key_file,
            Path::new("/run/credentials/controlroom.service/vapid")
        );
        // Without a credentials directory, a relative secret path is refused.
        let err = resolve("state_dir = \"/tmp\"\nmaster_key_file = \"master-key\"").unwrap_err();
        assert!(err.to_string().contains("CREDENTIALS_DIRECTORY"), "{err}");
    }

    #[test]
    fn flags_override_the_file() {
        let cli = Cli {
            listen: Some("127.0.0.1:9000".into()),
            auth_mode: Some(AuthModeArg::DevToken),
            public_url: Some("http://127.0.0.1:9000".into()),
            ..Cli::default()
        };
        let s = Config::from_toml(OIDC)
            .unwrap()
            .with_cli(&cli)
            .resolve(&Env::default())
            .unwrap();
        assert_eq!(s.listen, Listen::Tcp("127.0.0.1:9000".parse().unwrap()));
        assert!(matches!(s.auth, AuthSettings::DevToken));
    }

    #[test]
    fn the_client_id_can_come_from_a_credential() {
        let env = Env {
            credentials_directory: Some("/run/credentials/controlroom.service".into()),
            state_directory: None,
        };
        let from_file = OIDC.replace(
            "client_id = \"controlroom\"",
            "client_id_file = \"oidc-client-id\"",
        );
        let s = Config::from_toml(&from_file)
            .unwrap()
            .resolve(&env)
            .unwrap();
        let AuthSettings::Oidc(oidc) = s.auth else {
            panic!("oidc expected")
        };
        assert_eq!(
            oidc.client_id,
            ClientIdSource::File("/run/credentials/controlroom.service/oidc-client-id".into())
        );
        let both = OIDC.replace(
            "client_id = \"controlroom\"",
            "client_id = \"controlroom\"\nclient_id_file = \"/run/keys/id\"",
        );
        assert!(resolve(&both).is_err());
        let neither = OIDC.replace("client_id = \"controlroom\"", "");
        assert!(resolve(&neither).is_err());
    }

    #[test]
    fn unknown_keys_are_refused() {
        assert!(Config::from_toml("listne = \"127.0.0.1:1\"").is_err());
    }
}
