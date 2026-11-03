//! The raw configuration: the TOML file and the command line (each flag also reads an
//! environment variable). Every field is optional here; [`super::Config::resolve`] decides
//! what is required.

use std::path::PathBuf;

use serde::Deserialize;

/// The TOML file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// `IP:PORT` (default `127.0.0.1:8080`) or `unix:/path/to/socket`.
    pub listen: Option<String>,
    /// The origin browsers use, e.g. `https://controlroom.example.com`. Required except for
    /// dev-token mode on a loopback address.
    pub public_url: Option<String>,
    /// Where the client store and the dev token live (default: `$STATE_DIRECTORY`).
    pub state_dir: Option<PathBuf>,
    /// The master key file; relative paths are systemd credentials (default: the
    /// `master-key` credential).
    pub master_key_file: Option<PathBuf>,
    /// The bundled `controlroomd-<arch>-linux` binaries (default: the package's own).
    pub bundled_daemons_dir: Option<PathBuf>,
    /// How long a login lasts without use, in days (default 30, sliding).
    pub session_ttl_days: Option<u32>,
    /// Authentication.
    #[serde(default)]
    pub auth: AuthConfig,
    /// Web Push.
    #[serde(default)]
    pub push: PushConfig,
}

/// `[auth]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// `oidc` or `dev-token` (default: `oidc` when `[auth.oidc]` is present).
    pub mode: Option<AuthModeArg>,
    /// `[auth.oidc]`.
    pub oidc: Option<OidcConfig>,
}

/// The auth mode, as configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum AuthModeArg {
    /// OIDC login (deployments).
    Oidc,
    /// A random token printed at start (loopback development only).
    DevToken,
}

/// `[auth.oidc]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcConfig {
    /// The issuer URL; its discovery document is fetched from
    /// `<issuer>/.well-known/openid-configuration`.
    pub issuer: Option<String>,
    /// The client id.
    pub client_id: Option<String>,
    /// A file holding the client id instead (for identity providers whose client ids are
    /// generated secrets); relative paths are systemd credentials.
    pub client_id_file: Option<PathBuf>,
    /// The client secret file; relative paths are systemd credentials.
    pub client_secret_file: Option<PathBuf>,
    /// Subjects that may sign in.
    #[serde(default)]
    pub allowed_subjects: Vec<String>,
    /// Groups whose members may sign in (the `groups` claim; the `groups` scope is requested).
    #[serde(default)]
    pub allowed_groups: Vec<String>,
    /// Scopes beyond `openid`, `profile`, `email` (and `groups`).
    #[serde(default)]
    pub extra_scopes: Vec<String>,
}

/// `[push]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushConfig {
    /// The VAPID private key file (base64url P-256 scalar or PEM); relative paths are systemd
    /// credentials. Without it, Web Push is off.
    pub vapid_key_file: Option<PathBuf>,
    /// The VAPID contact, `mailto:` or `https:` (default: the public URL).
    pub subject: Option<String>,
}

/// Command-line flags; each also reads a `CONTROLROOM_WEB_*` environment variable and
/// overrides the file.
#[derive(Debug, Clone, Default, clap::Parser)]
#[command(
    name = "controlroom-web",
    version,
    about = "The controlroom web server"
)]
pub struct Cli {
    /// The TOML configuration file.
    #[arg(long, short = 'c', env = "CONTROLROOM_WEB_CONFIG")]
    pub config: Option<PathBuf>,
    /// `IP:PORT` or `unix:/path`.
    #[arg(long, env = "CONTROLROOM_WEB_LISTEN")]
    pub listen: Option<String>,
    /// The origin browsers use, e.g. `https://controlroom.example.com`.
    #[arg(long, env = "CONTROLROOM_WEB_PUBLIC_URL")]
    pub public_url: Option<String>,
    /// The state directory.
    #[arg(long, env = "CONTROLROOM_WEB_STATE_DIR")]
    pub state_dir: Option<PathBuf>,
    /// The master key file.
    #[arg(long, env = "CONTROLROOM_WEB_MASTER_KEY_FILE")]
    pub master_key_file: Option<PathBuf>,
    /// The auth mode.
    #[arg(long, value_enum, env = "CONTROLROOM_WEB_AUTH_MODE")]
    pub auth_mode: Option<AuthModeArg>,
    /// The bundled daemons directory.
    #[arg(long, env = "CONTROLROOM_WEB_BUNDLED_DAEMONS_DIR")]
    pub bundled_daemons_dir: Option<PathBuf>,
}

impl Config {
    /// Parses the TOML text.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Applies command-line and environment overrides.
    pub fn with_cli(mut self, cli: &Cli) -> Self {
        fn set<T: Clone>(slot: &mut Option<T>, value: &Option<T>) {
            if value.is_some() {
                slot.clone_from(value);
            }
        }
        set(&mut self.listen, &cli.listen);
        set(&mut self.public_url, &cli.public_url);
        set(&mut self.state_dir, &cli.state_dir);
        set(&mut self.master_key_file, &cli.master_key_file);
        set(&mut self.bundled_daemons_dir, &cli.bundled_daemons_dir);
        if cli.auth_mode.is_some() {
            self.auth.mode = cli.auth_mode;
        }
        self
    }
}
