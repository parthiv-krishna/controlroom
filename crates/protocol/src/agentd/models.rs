//! Model providers on the server (Settings › Models): API keys, custom OpenAI-compatible
//! gateways, subscription sign-in and the defaults of new sessions (`models.*`).
//!
//! Providers come from two places: the server's configuration (Nix: `sandbox.toml` routes,
//! pi's `models.json`, `auth.json` and `settings.json` in the agent daemon's agent dir), shown
//! read-only, and the ones managed here, stored by the agent daemon. The server's configuration
//! wins on a name clash ([`ModelProvider::clash`]). Changes apply to new sessions and the next
//! start of existing ones; running agents keep what they started with.
//!
//! **Secrets:** an API key travels once, in `models.set_key` or `models.set_gateway`, and is
//! never returned: results carry only its last four characters ([`StoredKey`]). Sign-in tokens
//! stay in the server's pi `auth.json`.
//!
//! **Sign-in** drives pi's own login flow on the server: `models.login_start` returns the first
//! step, `models.login_input` answers a step (a choice, or the pasted redirect URL),
//! `models.login_wait` returns once the step changes (long poll), `models.login_cancel` stops.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{LoginId, UnixMillis};
use crate::session::DeviceRef;

/// Where a provider's configuration comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSource {
    /// Not configured yet (a preset without a key or sign-in).
    None,
    /// Managed in Settings › Models.
    Ui,
    /// The server's configuration (read-only here).
    Server,
    /// A source this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// How a custom gateway takes its API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AuthHeader {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// `x-api-key: <key>`.
    XApiKey,
    /// `api-key: <key>` (Azure style).
    ApiKey,
    /// A style this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// What a stored key is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    /// A provider API key.
    #[default]
    ApiKey,
    /// A subscription token pasted in place of an API key (Anthropic's `sk-ant-oat…`, e.g. from
    /// `claude setup-token`): it takes the subscription's route and is never refreshed.
    SubscriptionToken,
    /// A kind this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// A stored API key, described without the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct StoredKey {
    /// What it is (`api_key` for keys stored before kinds existed).
    #[serde(default)]
    pub kind: KeyKind,
    /// Its last four characters.
    pub last4: String,
    /// When it was added or replaced.
    pub added_at: UnixMillis,
    /// The device that added it.
    pub added_by: Option<DeviceRef>,
}

/// A custom OpenAI-compatible gateway (pi's `openai-completions` API).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Gateway {
    /// The API base URL, e.g. `https://llm.example.com/v1`.
    pub base_url: String,
    /// The model ids it serves.
    pub models: Vec<String>,
    /// How it takes the key.
    pub auth_header: AuthHeader,
}

/// The last connection test of a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProviderTest {
    /// Whether it worked.
    pub ok: bool,
    /// What happened, e.g. `"HTTP 401 Unauthorized"`. Never contains a secret.
    pub message: String,
    /// When it ran.
    pub at: UnixMillis,
}

/// A subscription sign-in in the server's pi `auth.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SignedIn {
    /// The account, when the credential names one (e.g. an email address).
    pub account: Option<String>,
}

/// One model provider as Settings › Models shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelProvider {
    /// pi's provider id, e.g. `"anthropic"`.
    pub id: String,
    /// Display name.
    pub label: String,
    /// Where its configuration comes from.
    pub source: ProviderSource,
    /// It takes an API key here (a preset or a gateway).
    pub takes_key: bool,
    /// In place of an API key, it also takes a pasted subscription token with this prefix
    /// (`sk-ant-oat` for Anthropic).
    #[serde(default)]
    pub token_prefix: Option<String>,
    /// pi can sign in to it (a subscription).
    pub can_sign_in: bool,
    /// The API key stored here.
    pub key: Option<StoredKey>,
    /// The sign-in, when signed in.
    pub signed_in: Option<SignedIn>,
    /// A custom gateway's settings.
    pub gateway: Option<Gateway>,
    /// The last test.
    pub test: Option<ProviderTest>,
    /// Set when the server's configuration also defines this provider: it wins, and what is
    /// stored here is ignored until removed.
    pub clash: Option<String>,
}

/// A model new sessions can start with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelChoice {
    /// pi's provider id.
    pub provider: String,
    /// pi's model id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Whether it supports thinking levels.
    pub reasoning: bool,
}

/// The model and thinking level new sessions start with. `null` fields fall back (to the
/// server's defaults, then to pi's).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelDefaults {
    /// pi's provider id.
    pub provider: Option<String>,
    /// pi's model id.
    pub model: Option<String>,
    /// pi's thinking level (`off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`).
    pub thinking: Option<String>,
}

/// Result of every `models.*` method except sign-in: the whole Settings › Models state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelsState {
    /// The providers: presets, gateways and the server's.
    pub providers: Vec<ModelProvider>,
    /// The models pi offers with the configured providers.
    pub models: Vec<ModelChoice>,
    /// Why `models` may be incomplete (pi couldn't be asked).
    pub models_error: Option<String>,
    /// The defaults set here; they win over the server's.
    pub defaults: ModelDefaults,
    /// The defaults in the server's pi `settings.json`.
    pub server_defaults: ModelDefaults,
}

/// Params of `models.set_key`: add or replace a provider's API key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelsSetKeyParams {
    /// A preset provider or a gateway.
    pub provider: String,
    /// The API key, or a subscription token the provider takes ([`ModelProvider::token_prefix`]).
    /// Never returned.
    pub key: String,
    /// The device making the change; the controller fills it in (see
    /// [`super::RequestRespondParams::by`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<DeviceRef>,
}

/// Params of `models.set_gateway`: add or edit a custom gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelsSetGatewayParams {
    /// Its pi provider id: lowercase letters, digits and `-`, not a preset's.
    pub provider: String,
    /// Its settings.
    pub gateway: Gateway,
    /// The API key; required when adding, absent keeps the stored one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The device making the change (filled in by the controller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<DeviceRef>,
}

/// Params of `models.remove` (a key or a gateway), `models.sign_out` and `models.test`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelsProviderParams {
    /// The provider.
    pub provider: String,
    /// The device making the change (filled in by the controller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<DeviceRef>,
}

/// Params of `models.set_defaults`: replaces the defaults set here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelsSetDefaultsParams {
    /// The new defaults; all `null` clears them.
    pub defaults: ModelDefaults,
    /// The device making the change (filled in by the controller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<DeviceRef>,
}

/// Params of `models.login_start`: sign in to a subscription. A sign-in already running is
/// cancelled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct LoginStartParams {
    /// The provider.
    pub provider: String,
    /// The device making the change (filled in by the controller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<DeviceRef>,
}

/// Params of `models.login_input`: answer the current step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct LoginInputParams {
    /// The sign-in.
    pub login_id: LoginId,
    /// A [`LoginStep::Choose`] option id, a [`LoginStep::Input`] answer, or for
    /// [`LoginStep::OpenUrl`] the redirect URL (or code) the browser ended on.
    pub input: String,
    /// The device making the change (filled in by the controller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<DeviceRef>,
}

/// Params of `models.login_wait`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct LoginWaitParams {
    /// The sign-in.
    pub login_id: LoginId,
    /// Returns once [`LoginState::seq`] is past this, or after about 20 s with the current
    /// state.
    pub after: u32,
}

/// Params of `models.login_cancel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct LoginCancelParams {
    /// The sign-in.
    pub login_id: LoginId,
    /// The device making the change (filled in by the controller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<DeviceRef>,
}

/// One option of a [`LoginStep::Choose`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct LoginOption {
    /// What to send back.
    pub id: String,
    /// What to show.
    pub label: String,
}

/// Where a sign-in is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LoginStep {
    /// The flow is starting or working (exchanging a code, ...).
    Progress {
        /// What it is doing.
        message: String,
    },
    /// Pick one option (e.g. browser or device code).
    Choose {
        /// The question.
        message: String,
        /// The options.
        options: Vec<LoginOption>,
    },
    /// Open the sign-in page, then paste the URL the browser ends on (the callback listener
    /// is on the server, not where the browser runs).
    OpenUrl {
        /// The provider's sign-in page.
        url: String,
        /// The provider's instructions.
        instructions: Option<String>,
    },
    /// Enter the code at the verification page; the sign-in finishes by itself.
    DeviceCode {
        /// The code to enter.
        user_code: String,
        /// Where to enter it.
        verification_uri: String,
    },
    /// Type an answer.
    Input {
        /// The question.
        message: String,
        /// An example answer.
        placeholder: Option<String>,
    },
    /// Signed in.
    Done {
        /// The account, when the credential names one.
        account: Option<String>,
    },
    /// The sign-in failed.
    Failed {
        /// Why. Never contains a secret.
        message: String,
    },
    /// Cancelled (by the user or a newer sign-in).
    Cancelled,
    /// Not finished in time.
    TimedOut,
    /// A step this build doesn't know.
    #[serde(other)]
    Unknown,
}

impl LoginStep {
    /// Whether the sign-in is over.
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            LoginStep::Done { .. }
                | LoginStep::Failed { .. }
                | LoginStep::Cancelled
                | LoginStep::TimedOut
        )
    }
}

/// A sign-in and its current step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct LoginState {
    /// The sign-in.
    pub login_id: LoginId,
    /// The provider.
    pub provider: String,
    /// Increases with every step.
    pub seq: u32,
    /// The current step.
    pub step: LoginStep,
}
