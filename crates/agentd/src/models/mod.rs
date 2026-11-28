//! Settings › Models on the agent daemon: model providers managed from the UI next to the
//! server's own configuration (spec §11.3).
//!
//! - [`presets`]: the providers offered, with their verified routes;
//! - [`accounts`]: several named accounts of a provider, each a provider of its own for pi;
//! - [`store`]: what is stored (`<state>/models/`: metadata, the 0600 key store, the added
//!   accounts' sign-ins);
//! - [`server`]: what the server's configuration declares (read-only; it wins on a clash);
//! - [`merge`]: what a launch gets from here (routes, claims, keys, gateways, defaults);
//! - [`helper`], [`login`]: pi's own model list and sign-in, through the bundle's helper;
//! - [`probe`]: provider tests.
//!
//! [`Models`] serves the `models.*` methods. Changes apply to new sessions and to the next
//! start of existing ones; running agents are never restarted for them. Every change is logged
//! with the device that made it; keys are never logged or returned.

pub mod accounts;
pub mod helper;
pub mod login;
pub mod merge;
pub mod presets;
pub mod probe;
pub mod server;
pub mod store;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use controlroom_protocol::agentd::{
    AccountUsage, KeyKind, LoginCancelParams, LoginInputParams, LoginStartParams, LoginState,
    LoginStep, LoginWaitParams, ModelProvider, ModelsAccounts, ModelsAddAccountParams,
    ModelsProviderParams, ModelsRenameAccountParams, ModelsSetDefaultsParams,
    ModelsSetGatewayParams, ModelsSetKeyParams, ModelsState, ProviderSource, ProviderTest,
    SignedIn, StoredKey,
};
use controlroom_protocol::session::DeviceRef;
use controlroom_protocol::{ErrorData, Resource, RpcError, UnixMillis};
use controlroom_sandbox::agent_dir::{AgentDirSpec, build_agent_config};
use controlroom_sandbox::credential::{
    CredentialManifest, KeyStore, OAuthAccount, StoredKeys, resolve_credential,
};
use controlroom_sandbox::{AbsPath, Name};
use serde_json::{Map, Value};

use self::accounts::UsageSource;
use self::helper::{Catalog, Helper};
use self::login::{LOGIN_TIMEOUT, Logins};
use self::probe::ProbeRequest;
use self::server::{ServerConfig, signed_in_as};
use self::store::{Store, Stored, StoredProvider};
use crate::config::sandbox::{self as sandbox_config, SandboxSettings};
use crate::dirs::{Dirs, create_private_dir, home};
use crate::launch::find_program;
use crate::shell::login_shell;

/// Called with the provider (account) id whose credential or route changed.
pub type CredentialsChanged = Arc<dyn Fn(&str) + Send + Sync>;

/// pi's thinking levels.
pub const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// How long `models.login_wait` waits for the next step.
const LOGIN_WAIT: Duration = Duration::from_secs(20);

/// How long pi's model list is reused while nothing changed here (the server's files may).
const CATALOG_TTL: Duration = Duration::from_secs(60);

/// Longest API key accepted.
const MAX_KEY_LEN: usize = 4096;

/// The `models.*` service.
pub struct Models {
    dirs: Dirs,
    store: Store,
    /// Serializes changes to the store.
    writes: tokio::sync::Mutex<()>,
    logins: Logins,
    /// Bumped on every change; the catalog cache is valid for one generation.
    generation: Arc<AtomicU64>,
    catalog: tokio::sync::Mutex<Option<CachedCatalog>>,
    /// The helpers, once found (looked up again while missing).
    programs: Mutex<Programs>,
    /// The sessions' usage per provider, when sessions run here.
    usage: Option<UsageSource>,
    /// Told which provider's credential or route changed, when sessions run here.
    credentials: Option<CredentialsChanged>,
}

#[derive(Default)]
struct Programs {
    models: Option<AbsPath>,
    oauth: Option<AbsPath>,
}

struct CachedCatalog {
    generation: u64,
    at: Instant,
    catalog: Result<Catalog, String>,
}

/// Everything a request looks at, read fresh.
struct Context {
    sandbox: SandboxSettings,
    server: ServerConfig,
    stored: Stored,
    keys: KeyStore,
    /// The added accounts' own `auth.json`, by account id.
    account_auth: BTreeMap<String, Map<String, Value>>,
    /// Usage per provider id.
    usage: BTreeMap<String, AccountUsage>,
}

impl Context {
    /// `id`'s sign-in: in the agent daemon's pi `auth.json`, or for an added account in its own.
    fn sign_in(&self, id: &str) -> Option<SignedIn> {
        if self.stored.is_account(id) {
            let entry = self.account_auth.get(id)?.get(self.stored.base_of(id))?;
            (entry.get("type").and_then(Value::as_str) == Some("oauth")).then(|| SignedIn {
                account: signed_in_as(entry),
            })
        } else {
            (self.server.auth_type(id) == Some("oauth")).then(|| SignedIn {
                account: self.server.account(id),
            })
        }
    }

    fn signed_in(&self, id: &str) -> bool {
        self.sign_in(id).is_some()
    }
}

impl Models {
    /// The service for `dirs`.
    pub fn new(dirs: Dirs) -> Self {
        Self {
            store: Store::new(dirs.models()),
            dirs,
            writes: tokio::sync::Mutex::new(()),
            logins: Logins::default(),
            generation: Arc::new(AtomicU64::new(0)),
            catalog: tokio::sync::Mutex::new(None),
            programs: Mutex::new(Programs::default()),
            usage: None,
            credentials: None,
        }
    }

    /// Reports each account's usage from `source` (the sessions').
    pub fn with_usage(mut self, source: UsageSource) -> Self {
        self.usage = Some(source);
        self
    }

    /// Tells `changed` the provider whose credential or route changed (a key, gateway or
    /// sign-in set or removed), so running agents launched before can say they are stale.
    pub fn with_credential_changes(mut self, changed: CredentialsChanged) -> Self {
        self.credentials = Some(changed);
        self
    }

    /// `models.list`.
    pub async fn list(&self) -> Result<ModelsState, RpcError> {
        let ctx = self.context()?;
        Ok(self.state(&ctx).await)
    }

    /// `models.set_key`.
    pub async fn set_key(&self, p: ModelsSetKeyParams) -> Result<ModelsState, RpcError> {
        let key = valid_key(&p.key)?;
        let id = p.provider.as_str();
        let _write = self.writes.lock().await;
        let ctx = self.context()?;
        let base = ctx.stored.base_of(id).to_owned();
        let kind = presets::check_key(&base, key).map_err(invalid)?;
        refuse_server_declared(&ctx.server, id)?;
        let is_gateway = ctx
            .stored
            .providers
            .get(&base)
            .is_some_and(|s| s.gateway.is_some());
        let preset = presets::preset(&base);
        if !is_gateway && preset.and_then(|p| p.api_key).is_none() {
            return Err(invalid(format!(
                "{base} doesn't take an API key here; add it as a custom gateway"
            )));
        }
        if ctx.signed_in(id) {
            return Err(invalid(format!(
                "signed in to {id}, which pi prefers over a key; sign out first"
            )));
        }
        let mut keys = ctx.keys;
        keys.insert(id, key);
        self.store.save_keys(&keys).map_err(internal)?;
        let mut stored = ctx.stored;
        let entry = stored.providers.entry(id.to_owned()).or_default();
        entry.key = Some(stored_key(key, kind, &p.by));
        entry.test = None;
        self.store.save(&stored).map_err(internal)?;
        let what = match kind {
            KeyKind::SubscriptionToken => "subscription token set",
            _ => "API key set",
        };
        self.changed(&p.by, id, what);
        self.credential_changed(id);
        self.list().await
    }

    /// `models.set_gateway`.
    pub async fn set_gateway(&self, p: ModelsSetGatewayParams) -> Result<ModelsState, RpcError> {
        let id = p.provider.as_str();
        valid_gateway_id(id)?;
        let key = p.key.as_deref().map(valid_key).transpose()?;
        let kind = key
            .map(|key| presets::check_key(id, key))
            .transpose()
            .map_err(invalid)?
            .unwrap_or_default();
        let mut gateway = p.gateway;
        gateway.models = valid_models(&gateway.models)?;
        gateway.base_url = gateway.base_url.trim().to_owned();
        let _write = self.writes.lock().await;
        let ctx = self.context()?;
        refuse_server_declared(&ctx.server, id)?;
        if ctx.stored.is_account(id) {
            return Err(invalid(format!(
                "{id} is an account of {}; edit that gateway",
                ctx.stored.base_of(id)
            )));
        }
        merge::gateway_route(id, &gateway)
            .map_err(invalid)?
            .resolve(ctx.sandbox.allow_loopback_upstreams)
            .map_err(|e| invalid(e.to_string()))?;
        let mut keys = ctx.keys;
        let mut stored = ctx.stored;
        let entry = stored.providers.entry(id.to_owned()).or_default();
        match key {
            Some(key) => {
                keys.insert(id, key);
                self.store.save_keys(&keys).map_err(internal)?;
                entry.key = Some(stored_key(key, kind, &p.by));
            }
            None if keys.contains(id) => {}
            None => return Err(invalid("a new gateway needs its API key")),
        }
        entry.gateway = Some(gateway);
        entry.test = None;
        self.store.save(&stored).map_err(internal)?;
        self.changed(&p.by, id, "gateway saved");
        self.credential_changed(id);
        self.list().await
    }

    /// `models.remove`: a stored key, a gateway with its key, or an added account with its key
    /// or sign-in. A provider's first account keeps its name.
    pub async fn remove(&self, p: ModelsProviderParams) -> Result<ModelsState, RpcError> {
        let id = p.provider.as_str();
        let _write = self.writes.lock().await;
        let ctx = self.context()?;
        let mut stored = ctx.stored;
        let mut keys = ctx.keys;
        let account = stored.is_account(id);
        let entry = stored.providers.remove(id).unwrap_or_default();
        if entry.gateway.is_some() && stored.accounts_of(id).next().is_some() {
            return Err(invalid(format!(
                "the gateway {id} has other accounts; remove them first"
            )));
        }
        let had_key = keys.remove(id);
        if !account && !had_key && entry.key.is_none() && entry.gateway.is_none() {
            return Err(not_found(Resource::Provider, id));
        }
        if account {
            self.store.remove_account_dir(id).map_err(internal)?;
            if stored.defaults.provider.as_deref() == Some(id) {
                stored.defaults.provider = None;
                stored.defaults.model = None;
            }
        } else if entry.name.is_some() {
            let name = entry.name;
            stored.providers.insert(
                id.to_owned(),
                StoredProvider {
                    name,
                    ..StoredProvider::default()
                },
            );
        }
        self.store.save_keys(&keys).map_err(internal)?;
        self.store.save(&stored).map_err(internal)?;
        self.changed(
            &p.by,
            id,
            if account {
                "account removed"
            } else {
                "removed"
            },
        );
        self.credential_changed(id);
        self.list().await
    }

    /// `models.sign_out`.
    pub async fn sign_out(&self, p: ModelsProviderParams) -> Result<ModelsState, RpcError> {
        let id = p.provider.as_str();
        let _write = self.writes.lock().await;
        let ctx = self.context()?;
        if !ctx.signed_in(id) {
            return Err(invalid(format!("not signed in to {id}")));
        }
        let (dir, signed_in) = self.login_dir(&ctx, id)?;
        let helper = self.models_helper(&ctx.sandbox)?;
        helper.logout(&dir, &signed_in).await.map_err(internal)?;
        self.changed(&p.by, id, "signed out");
        self.credential_changed(id);
        self.list().await
    }

    /// `models.add_account`: a named account of a preset or gateway, without a credential yet.
    pub async fn add_account(&self, p: ModelsAddAccountParams) -> Result<ModelsState, RpcError> {
        let name = accounts::valid_name(&p.name).map_err(invalid)?;
        let base = p.provider.as_str();
        let _write = self.writes.lock().await;
        let ctx = self.context()?;
        if ctx.stored.is_account(base) {
            return Err(invalid(format!(
                "{base} is an account; add accounts to {}",
                ctx.stored.base_of(base)
            )));
        }
        if !accounts::can_have_accounts(&ctx.stored, base) {
            return Err(invalid(format!(
                "{base} can't have accounts here (presets and gateways can)"
            )));
        }
        refuse_duplicate_name(&ctx.stored, base, None, &name)?;
        let known: Vec<String> = match self.catalog(&ctx).await {
            Ok(catalog) => catalog.providers.into_iter().map(|p| p.id).collect(),
            Err(_) => Vec::new(),
        };
        let id = accounts::new_id(base, &name, |id| {
            presets::preset(id).is_some()
                || ctx.stored.providers.contains_key(id)
                || ctx.keys.contains(id)
                || ctx.server.declares(id).is_some()
                || ctx.server.auth.contains_key(id)
                || known.iter().any(|k| k == id)
        });
        Name::new(&id).map_err(|e| invalid(e.to_string()))?;
        let mut stored = ctx.stored;
        stored.providers.insert(
            id.clone(),
            StoredProvider {
                account_of: Some(base.to_owned()),
                name: Some(name.clone()),
                ..StoredProvider::default()
            },
        );
        self.store.save(&stored).map_err(internal)?;
        self.changed(&p.by, &id, &format!("account {name} added to {base}"));
        self.list().await
    }

    /// `models.rename_account`.
    pub async fn rename_account(
        &self,
        p: ModelsRenameAccountParams,
    ) -> Result<ModelsState, RpcError> {
        let name = accounts::valid_name(&p.name).map_err(invalid)?;
        let id = p.provider.as_str();
        let _write = self.writes.lock().await;
        let mut stored = self.context()?.stored;
        let base = stored.base_of(id).to_owned();
        if !stored.is_account(id) && !accounts::can_have_accounts(&stored, id) {
            return Err(not_found(Resource::Provider, id));
        }
        refuse_duplicate_name(&stored, &base, Some(id), &name)?;
        stored.providers.entry(id.to_owned()).or_default().name = Some(name.clone());
        self.store.save(&stored).map_err(internal)?;
        self.changed(&p.by, id, &format!("account renamed to {name}"));
        self.list().await
    }

    /// `models.accounts`.
    pub fn accounts(&self) -> Result<ModelsAccounts, RpcError> {
        let stored = self.store.load().map_err(internal)?;
        Ok(ModelsAccounts {
            accounts: accounts::list(&stored),
        })
    }

    /// `models.set_defaults`.
    pub async fn set_defaults(&self, p: ModelsSetDefaultsParams) -> Result<ModelsState, RpcError> {
        let mut defaults = p.defaults;
        for value in [
            &mut defaults.provider,
            &mut defaults.model,
            &mut defaults.thinking,
        ] {
            *value = value
                .take()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty());
        }
        if defaults.model.is_some() && defaults.provider.is_none() {
            return Err(invalid("a default model needs its provider"));
        }
        if let Some(level) = &defaults.thinking
            && !THINKING_LEVELS.contains(&level.as_str())
        {
            return Err(invalid(format!("unknown thinking level {level:?}")));
        }
        let _write = self.writes.lock().await;
        let mut stored = self.context()?.stored;
        stored.defaults = defaults;
        self.store.save(&stored).map_err(internal)?;
        self.changed(&p.by, "-", "defaults set");
        self.list().await
    }

    /// `models.test`: one cheap request with the provider's credential, through its route's
    /// upstream and header; a sign-in is tested by getting a fresh token. A pasted subscription
    /// token is tested as it is (it is never refreshed). The result is kept.
    pub async fn test(&self, p: ModelsProviderParams) -> Result<ModelsState, RpcError> {
        let id = p.provider.clone();
        let ctx = self.context()?;
        let manifest = self.manifest(&ctx)?;
        let target = probe_target(&ctx, &id)?;
        let provider = Name::new(&id).map_err(|e| invalid(e.to_string()))?;
        let secret =
            tokio::task::spawn_blocking(move || resolve_credential(&manifest, &provider, 60))
                .await
                .map_err(internal)?;
        let outcome = match (secret, target) {
            (Err(e), _) => Err(e.to_string()),
            (Ok(_), None) => Ok("signed in; the token is valid".to_owned()),
            (Ok(secret), Some(request)) => probe::probe(&request, secret.expose()).await,
        };
        let test = ProviderTest {
            ok: outcome.is_ok(),
            message: outcome.unwrap_or_else(|e| e),
            at: UnixMillis::now(),
        };
        tracing::info!(provider = %id, by = %device(&p.by), ok = test.ok, "models: tested: {}", test.message);
        let _write = self.writes.lock().await;
        let mut stored = self.context()?.stored;
        stored.providers.entry(id).or_default().test = Some(test);
        self.store.save(&stored).map_err(internal)?;
        self.list().await
    }

    /// `models.login_start`.
    pub async fn login_start(&self, p: LoginStartParams) -> Result<LoginState, RpcError> {
        let id = p.provider.as_str();
        let ctx = self.context()?;
        let catalog = self.catalog(&ctx).await;
        if !can_sign_in(&ctx, catalog.as_ref().ok(), id) {
            return Err(invalid(format!("pi can't sign in to {id} here")));
        }
        if ctx.keys.contains(id) {
            return Err(invalid(format!(
                "{id} has an API key here, which a sign-in would replace; remove it first"
            )));
        }
        let (dir, signed_in) = self.login_dir(&ctx, id)?;
        if ctx.stored.is_account(id) {
            self.store.create_account_dir(id).map_err(internal)?;
        } else {
            create_private_dir(&dir).map_err(internal)?;
        }
        let mut command = self.models_helper(&ctx.sandbox)?.command("login", &dir);
        command.arg("--provider").arg(&signed_in);
        let generation = self.generation.clone();
        let credentials = self.credentials.clone();
        let (provider, by) = (id.to_owned(), device(&p.by));
        let state = self
            .logins
            .start(
                id,
                command,
                LOGIN_TIMEOUT,
                Box::new(move |state| {
                    generation.fetch_add(1, Ordering::SeqCst);
                    if let (LoginStep::Done { .. }, Some(changed)) = (&state.step, &credentials) {
                        changed(&provider);
                    }
                    tracing::info!(%provider, %by, "models: sign-in ended: {}", outcome(&state.step));
                }),
            )
            .map_err(internal)?;
        tracing::info!(provider = %id, by = %device(&p.by), "models: sign-in started");
        Ok(state)
    }

    /// `models.login_input`.
    pub async fn login_input(&self, p: LoginInputParams) -> Result<LoginState, RpcError> {
        self.logins.input(p.login_id, p.input).await
    }

    /// `models.login_wait`.
    pub async fn login_wait(&self, p: LoginWaitParams) -> Result<LoginState, RpcError> {
        self.logins.wait(p.login_id, p.after, LOGIN_WAIT).await
    }

    /// `models.login_cancel`.
    pub async fn login_cancel(&self, p: LoginCancelParams) -> Result<LoginState, RpcError> {
        tracing::info!(by = %device(&p.by), "models: sign-in cancelled");
        self.logins.cancel(p.login_id).await
    }

    // ---------------------------------------------------------------------------------------

    fn context(&self) -> Result<Context, RpcError> {
        let loaded = sandbox_config::load(&self.dirs);
        let sandbox = loaded
            .value
            .resolve(home().as_ref(), &self.dirs.pi_agent_dir())
            .map_err(internal)?;
        let keys = self.store.keys().map_err(internal)?;
        let mut stored = self.store.load().map_err(internal)?;
        stored.classify_keys(&keys);
        let account_auth = stored
            .providers
            .iter()
            .filter(|(_, p)| p.account_of.is_some())
            .map(|(id, _)| (id.clone(), self.store.account_auth(id)))
            .collect();
        let usage = self
            .usage
            .as_ref()
            .map(|source| accounts::usage(source, UnixMillis::now()))
            .unwrap_or_default();
        Ok(Context {
            server: ServerConfig::load(&sandbox),
            stored,
            keys,
            account_auth,
            usage,
            sandbox,
        })
    }

    /// Where `id` signs in, and the provider pi signs in to there: the agent daemon's agent dir
    /// for a provider's first account, its own directory for an added one.
    fn login_dir(&self, ctx: &Context, id: &str) -> Result<(AbsPath, String), RpcError> {
        if ctx.stored.is_account(id) {
            Ok((
                self.store.account_dir(id),
                ctx.stored.base_of(id).to_owned(),
            ))
        } else {
            Ok((agent_dir(&ctx.sandbox)?, id.to_owned()))
        }
    }

    /// Tells running agents' summaries that `provider`'s credential or route changed.
    fn credential_changed(&self, provider: &str) {
        if let Some(changed) = &self.credentials {
            changed(provider);
        }
    }

    /// Logs a change and invalidates the model list.
    fn changed(&self, by: &Option<DeviceRef>, provider: &str, what: &str) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        tracing::info!(%provider, by = %device(by), "models: {what}");
    }

    /// pi's view of the providers: their config as a session gets it, asked from pi itself.
    async fn catalog(&self, ctx: &Context) -> Result<Catalog, String> {
        let generation = self.generation.load(Ordering::SeqCst);
        let mut cache = self.catalog.lock().await;
        if let Some(cached) = cache.as_ref()
            && cached.generation == generation
            && cached.at.elapsed() < CATALOG_TTL
        {
            return cached.catalog.clone();
        }
        let catalog = self.fresh_catalog(ctx).await;
        *cache = Some(CachedCatalog {
            generation,
            at: Instant::now(),
            catalog: catalog.clone(),
        });
        catalog
    }

    /// pi's catalog of the merged config. Also keeps the model definitions of presets with
    /// added accounts up to date, which the accounts' providers are defined with: when they
    /// changed, the catalog is asked again with them.
    async fn fresh_catalog(&self, ctx: &Context) -> Result<Catalog, String> {
        let helper = self.models_helper(&ctx.sandbox).map_err(|e| e.message)?;
        let mut bases: Vec<String> = ctx
            .stored
            .providers
            .values()
            .filter_map(|p| p.account_of.clone())
            .filter(|base| presets::preset(base).is_some())
            .collect();
        bases.sort();
        bases.dedup();
        let catalog = self.list_catalog(ctx, &helper, &bases).await?;
        let mut definitions = self.store.definitions();
        let changed = bases
            .iter()
            .any(|base| catalog.definitions.get(base) != definitions.get(base));
        if !changed {
            return Ok(catalog);
        }
        for base in &bases {
            match catalog.definitions.get(base) {
                Some(models) => definitions.insert(base.clone(), models.clone()),
                None => definitions.remove(base),
            };
        }
        self.store.save_definitions(&definitions)?;
        self.list_catalog(ctx, &helper, &[]).await
    }

    async fn list_catalog(
        &self,
        ctx: &Context,
        helper: &Helper,
        definitions: &[String],
    ) -> Result<Catalog, String> {
        let mut sandbox = ctx.sandbox.clone();
        merge::apply(&self.dirs, &mut sandbox);
        let dir = self.store.catalog_dir();
        let spec = AgentDirSpec {
            oauth_helper: None,
            ..sandbox.agent_dir
        };
        build_agent_config(&dir, &spec).map_err(|e| e.to_string())?;
        helper.list(&dir, definitions).await
    }

    async fn state(&self, ctx: &Context) -> ModelsState {
        let catalog = self.catalog(ctx).await;
        let (providers, models, models_error) = match &catalog {
            Ok(c) => (rows(ctx, Some(c)), c.models.clone(), None),
            Err(e) => (rows(ctx, None), Vec::new(), Some(e.clone())),
        };
        ModelsState {
            providers,
            models,
            models_error,
            defaults: ctx.stored.defaults.clone(),
            server_defaults: ctx.server.defaults.clone(),
        }
    }

    /// The credential manifest a session would get, for tests.
    fn manifest(&self, ctx: &Context) -> Result<CredentialManifest, RpcError> {
        let agent_dir = agent_dir(&ctx.sandbox)?;
        let api_key_providers = ctx
            .server
            .declared()
            .into_iter()
            .filter(|p| ctx.server.has_key(p))
            .collect();
        let stored = ctx
            .keys
            .providers()
            .filter(|p| ctx.server.declares(p).is_none())
            .map(str::to_owned)
            .collect();
        let oauth_accounts = ctx
            .account_auth
            .keys()
            .filter(|id| ctx.signed_in(id))
            .map(|id| {
                let account = OAuthAccount {
                    agent_dir: self.store.account_dir(id),
                    provider: ctx.stored.base_of(id).to_owned(),
                };
                (id.clone(), account)
            })
            .collect();
        Ok(CredentialManifest {
            agent_dir,
            api_key_providers,
            oauth_providers: ctx.server.signed_in(),
            oauth_helper: self.oauth_helper(&ctx.sandbox),
            stored_keys: Some(StoredKeys {
                path: self.store.keys_path(),
                providers: stored,
            }),
            oauth_accounts,
        })
    }

    fn models_helper(&self, sandbox: &SandboxSettings) -> Result<Helper, RpcError> {
        let mut programs = self.programs.lock().unwrap_or_else(PoisonError::into_inner);
        if programs.models.is_none() {
            programs.models = Some(
                find_program(&login_shell(), &sandbox.models_helper)
                    .map_err(|e| unsupported(format!("the models helper: {e}")))?,
            );
        }
        programs
            .models
            .clone()
            .map(Helper::new)
            .ok_or_else(|| internal("the models helper"))
    }

    fn oauth_helper(&self, sandbox: &SandboxSettings) -> Option<AbsPath> {
        let mut programs = self.programs.lock().unwrap_or_else(PoisonError::into_inner);
        if programs.oauth.is_none() {
            programs.oauth = find_program(&login_shell(), &sandbox.oauth_helper).ok();
        }
        programs.oauth.clone()
    }
}

/// The rows of Settings › Models: presets, gateways, the server's providers, other sign-ins,
/// each provider followed by the accounts added to it.
fn rows(ctx: &Context, catalog: Option<&Catalog>) -> Vec<ModelProvider> {
    let mut ids: Vec<String> = presets::PRESETS.iter().map(|p| p.id.to_owned()).collect();
    let mut more: Vec<String> = ctx
        .stored
        .providers
        .iter()
        .filter(|(_, s)| s.gateway.is_some())
        .map(|(id, _)| id.clone())
        .chain(ctx.server.declared())
        .chain(ctx.server.signed_in())
        .filter(|id| !ids.contains(id) && !ctx.stored.is_account(id))
        .collect();
    more.sort();
    more.dedup();
    ids.extend(more);
    ids.iter()
        .flat_map(|id| std::iter::once(id.as_str()).chain(ctx.stored.accounts_of(id)))
        .map(|id| row(ctx, catalog, id))
        .collect()
}

fn row(ctx: &Context, catalog: Option<&Catalog>, id: &str) -> ModelProvider {
    let account = ctx.stored.is_account(id);
    let base = ctx.stored.base_of(id);
    let preset = presets::preset(base);
    let stored = ctx.stored.providers.get(id).cloned().unwrap_or_default();
    let base_gateway = ctx
        .stored
        .providers
        .get(base)
        .and_then(|p| p.gateway.as_ref());
    let declared = ctx.server.declares(id);
    let managed = stored.key.is_some() || stored.gateway.is_some() || account;
    let label = preset
        .map(|p| p.label.to_owned())
        .or_else(|| {
            let c = catalog?.providers.iter().find(|p| p.id == base)?;
            Some(c.name.clone())
        })
        .unwrap_or_else(|| base.to_owned());
    let can_have_accounts = accounts::can_have_accounts(&ctx.stored, base);
    ModelProvider {
        id: id.to_owned(),
        label,
        source: match (declared, managed) {
            (Some(_), _) => ProviderSource::Server,
            (None, true) => ProviderSource::Ui,
            (None, false) => ProviderSource::None,
        },
        takes_key: declared.is_none()
            && (preset.and_then(|p| p.api_key).is_some() || base_gateway.is_some()),
        token_prefix: presets::pasted_token(base)
            .filter(|_| declared.is_none())
            .map(|t| t.prefix.to_owned()),
        can_sign_in: can_sign_in(ctx, catalog, id),
        key: stored.key,
        signed_in: ctx.sign_in(id),
        gateway: stored.gateway,
        test: stored.test,
        clash: declared
            .filter(|_| managed)
            .map(|why| format!("The server's config wins: {why}")),
        account: can_have_accounts.then(|| ctx.stored.account_name(id)),
        account_of: account.then(|| base.to_owned()),
        can_add_account: !account && can_have_accounts,
        usage: ctx.usage.get(id).copied(),
    }
}

/// Whether pi can sign in to `id` and sessions could then use it: a subscription preset (or an
/// account of one), or a server-declared provider pi has a sign-in for.
fn can_sign_in(ctx: &Context, catalog: Option<&Catalog>, id: &str) -> bool {
    let base = ctx.stored.base_of(id);
    let preset = presets::preset(base).and_then(|p| p.subscription).is_some();
    let server = !ctx.stored.is_account(id)
        && ctx.server.route(id).is_some()
        && catalog.is_some_and(|c| c.providers.iter().any(|p| p.id == id && p.oauth));
    preset || server
}

/// What `models.test` requests for `id` (a provider, or an account of one): `None` for a
/// sign-in (a fresh token is the test).
fn probe_target(ctx: &Context, id: &str) -> Result<Option<ProbeRequest>, RpcError> {
    if ctx.signed_in(id) {
        return Ok(None);
    }
    let base = ctx.stored.base_of(id);
    let url = |origin: &str, path: &str| format!("{}{path}", origin.trim_end_matches('/'));
    let stored = ctx.stored.providers.get(id);
    let declared = ctx.server.declares(id).is_some();
    let preset = presets::preset(base).and_then(|p| p.api_key);
    if let Some(route) = ctx.server.route(id) {
        // The server's route; the preset's test path, or a gateway's model list.
        let path = match (preset, ctx.server.models.get(id)) {
            (Some((_, probe)), _) => probe.path.to_owned(),
            (None, Some(provider)) => {
                let base_url = provider
                    .get("baseUrl")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let (_, path) = merge::split_base_url(base_url).map_err(invalid)?;
                format!("{path}/models")
            }
            (None, None) => return Err(unsupported(format!("no test for {id}"))),
        };
        return Ok(Some(ProbeRequest {
            url: url(route.upstream.as_str(), &path),
            header: route
                .inject_header
                .clone()
                .unwrap_or_else(|| "Authorization".to_owned()),
            format: route
                .credential_format
                .clone()
                .unwrap_or_else(|| "Bearer {}".to_owned()),
            extra: preset
                .map(|(_, probe)| probe.headers)
                .unwrap_or_default()
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            unauthorized: None,
        }));
    }
    if declared {
        return Err(unsupported(format!("{id} has no route on this server")));
    }
    if let Some(gateway) = ctx
        .stored
        .providers
        .get(base)
        .and_then(|s| s.gateway.as_ref())
    {
        let (header, format) = presets::gateway_header(gateway.auth_header)
            .ok_or_else(|| invalid("unknown auth header style"))?;
        return Ok(Some(ProbeRequest {
            url: format!("{}/models", gateway.base_url.trim_end_matches('/')),
            header: header.to_owned(),
            format: format.to_owned(),
            extra: Vec::new(),
            unauthorized: None,
        }));
    }
    let extra = |probe: presets::Probe| -> Vec<(String, String)> {
        probe
            .headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    };
    if stored.and_then(|s| s.key.as_ref()).map(|k| k.kind) == Some(KeyKind::SubscriptionToken)
        && let Some(sub) = presets::preset(base).and_then(|p| p.subscription)
        && let Some(token) = sub.pasted
    {
        return Ok(Some(ProbeRequest {
            url: url(sub.route.upstream, token.probe.path),
            header: sub.route.header.to_owned(),
            format: sub.route.format.to_owned(),
            extra: extra(token.probe),
            unauthorized: Some("token rejected (expired or revoked?)"),
        }));
    }
    let (route, probe) = preset.ok_or_else(|| not_found(Resource::Provider, id))?;
    Ok(Some(ProbeRequest {
        url: url(route.upstream, probe.path),
        header: route.header.to_owned(),
        format: route.format.to_owned(),
        extra: extra(probe),
        unauthorized: None,
    }))
}

fn agent_dir(sandbox: &SandboxSettings) -> Result<AbsPath, RpcError> {
    sandbox
        .agent_dir
        .user_agent_dir
        .clone()
        .ok_or_else(|| internal("no pi agent dir"))
}

/// Refuses `name` for an account of `base` when another of its accounts (than `except`) has
/// it.
fn refuse_duplicate_name(
    stored: &Stored,
    base: &str,
    except: Option<&str>,
    name: &str,
) -> Result<(), RpcError> {
    let taken = std::iter::once(base)
        .chain(stored.accounts_of(base))
        .filter(|id| Some(*id) != except)
        .any(|id| stored.account_name(id) == name);
    if taken {
        return Err(invalid(format!(
            "{base} already has an account named {name}"
        )));
    }
    Ok(())
}

fn refuse_server_declared(server: &ServerConfig, id: &str) -> Result<(), RpcError> {
    match server.declares(id) {
        Some(why) => Err(invalid(format!(
            "{id} is in the server's configuration ({why}); change it there"
        ))),
        None => Ok(()),
    }
}

fn stored_key(key: &str, kind: KeyKind, by: &Option<DeviceRef>) -> StoredKey {
    let tail: Vec<char> = key.chars().rev().take(4).collect();
    StoredKey {
        kind,
        last4: tail.into_iter().rev().collect(),
        added_at: UnixMillis::now(),
        added_by: by.clone(),
    }
}

fn valid_key(key: &str) -> Result<&str, RpcError> {
    let key = key.trim();
    if key.is_empty() || key.len() > MAX_KEY_LEN || key.chars().any(char::is_whitespace) {
        return Err(invalid("an API key is one line without spaces"));
    }
    Ok(key)
}

fn valid_gateway_id(id: &str) -> Result<(), RpcError> {
    let ok = (1..=40).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !ok {
        return Err(invalid(
            "a gateway name is 1-40 lowercase letters, digits and dashes",
        ));
    }
    if presets::preset(id).is_some() {
        return Err(invalid(format!("{id} is a preset; pick another name")));
    }
    Ok(())
}

fn valid_models(models: &[String]) -> Result<Vec<String>, RpcError> {
    let mut out: Vec<String> = Vec::new();
    for model in models.iter().map(|m| m.trim()) {
        if model.is_empty() || model.len() > 200 || model.chars().any(char::is_whitespace) {
            return Err(invalid(format!("invalid model id {model:?}")));
        }
        if !out.iter().any(|m| m == model) {
            out.push(model.to_owned());
        }
    }
    if out.is_empty() {
        return Err(invalid("a gateway needs at least one model"));
    }
    Ok(out)
}

/// A sign-in's final step, for the log.
fn outcome(step: &LoginStep) -> String {
    match step {
        LoginStep::Done { account } => match account {
            Some(account) => format!("signed in as {account}"),
            None => "signed in".to_owned(),
        },
        LoginStep::Failed { message } => format!("failed: {message}"),
        LoginStep::Cancelled => "cancelled".to_owned(),
        LoginStep::TimedOut => "timed out".to_owned(),
        _ => "stopped".to_owned(),
    }
}

/// The device for the log.
fn device(by: &Option<DeviceRef>) -> String {
    by.as_ref().map_or_else(
        || "a local client".to_owned(),
        |d| format!("{} ({})", d.label, d.device_id),
    )
}

fn invalid(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorData::InvalidParams, message.into())
}

fn unsupported(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorData::Unsupported, message.into())
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorData::InternalError, e.to_string())
}

fn not_found(resource: Resource, id: &str) -> RpcError {
    RpcError::new(
        ErrorData::NotFound {
            resource,
            id: id.to_owned(),
        },
        format!("{id} has nothing stored here"),
    )
}
