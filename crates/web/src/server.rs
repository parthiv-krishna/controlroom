//! Startup: secrets, the controller, the listener and the router.

use std::net::SocketAddr;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use axum::Router;
use axum::http::HeaderValue;
use axum::middleware::from_fn_with_state;
use axum::routing::{any, get, post};
use controlroom_controller::{
    AuthPolicy, Controller, ControllerConfig, MasterKey, PushConfig, Tuning,
};
use controlroom_protocol::ui::AuthMode;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::auth::cookies::Cookies;
use crate::auth::dev_token::{self, DevToken};
use crate::auth::oidc::{self, Oidc};
use crate::auth::{self, AuthRuntime};
use crate::config::{AuthSettings, ClientIdSource, Listen, Settings};
use crate::csp::Csp;
use crate::hardening;
use crate::origin::OriginPolicy;
use crate::secrets::{ensure_private_dir, read_text_secret};
use crate::static_files::{self, Assets};
use crate::{terminal, ws};

/// The server's version, reported in `hello` and to daemons.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The client store, in the state directory.
pub const STORE_FILE: &str = "controlroom.sqlite";
/// How often expired login sessions are deleted.
const PURGE_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// How long shutdown waits for in-flight HTTP requests.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// What every request handler shares.
pub(crate) struct AppState {
    pub controller: Controller,
    pub origin: OriginPolicy,
    pub csp: HeaderValue,
    pub cookies: Cookies,
    pub session_ttl: Duration,
    pub auth: AuthRuntime,
    pub assets: Assets,
}

/// Overrides for embedding and tests.
#[derive(Clone)]
pub struct StartOptions {
    /// The UI files; default: the embedded build.
    pub assets: Assets,
}

impl Default for StartOptions {
    fn default() -> Self {
        Self {
            assets: Assets::Embedded,
        }
    }
}

/// A running server.
pub struct Server {
    local_addr: Option<SocketAddr>,
    public_origin: String,
    dev_login_url: Option<String>,
    controller: Controller,
    shutdown: oneshot::Sender<()>,
    serve: JoinHandle<std::io::Result<()>>,
    purge: JoinHandle<()>,
}

impl Server {
    /// The bound TCP address (`None` on a unix socket).
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.local_addr
    }

    /// The origin browsers use.
    pub fn public_origin(&self) -> &str {
        &self.public_origin
    }

    /// In dev-token mode, the URL that signs a browser in.
    pub fn dev_login_url(&self) -> Option<&str> {
        self.dev_login_url.as_deref()
    }

    /// The controller.
    pub fn controller(&self) -> &Controller {
        &self.controller
    }

    /// Stops accepting connections, closes every WebSocket (code 1001) and machine
    /// connection, and waits for in-flight requests.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        let _ = self.shutdown.send(());
        self.controller.shutdown().await;
        self.purge.abort();
        match tokio::time::timeout(SHUTDOWN_GRACE, self.serve).await {
            Ok(result) => result.context("the server task panicked")??,
            Err(_) => tracing::warn!("in-flight requests didn't finish in time"),
        }
        Ok(())
    }
}

enum Listener {
    Tcp(tokio::net::TcpListener),
    Unix(tokio::net::UnixListener),
}

async fn bind(listen: &Listen) -> anyhow::Result<Listener> {
    match listen {
        Listen::Tcp(addr) => Ok(Listener::Tcp(
            tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("binding {addr}"))?,
        )),
        Listen::Unix(path) => {
            // A stale socket from an earlier run is replaced; anything else is left alone.
            if let Ok(meta) = std::fs::symlink_metadata(path)
                && meta.file_type().is_socket()
            {
                std::fs::remove_file(path)?;
            }
            let listener = tokio::net::UnixListener::bind(path)
                .with_context(|| format!("binding {listen}"))?;
            // The reverse proxy connects through the group.
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
            Ok(Listener::Unix(listener))
        }
    }
}

/// Loads the secrets, starts the controller, binds the listener and starts serving.
/// Refuses to start without a usable master key.
pub async fn start(settings: Settings, options: StartOptions) -> anyhow::Result<Server> {
    let master_key = MasterKey::load_file(&settings.master_key_file)
        .map_err(|e| anyhow::anyhow!("refusing to start: {e}"))?;
    ensure_private_dir(&settings.state_dir).with_context(|| {
        format!(
            "creating the state directory {}",
            settings.state_dir.display()
        )
    })?;

    let listener = bind(&settings.listen).await?;
    let local_addr = match &listener {
        Listener::Tcp(l) => Some(l.local_addr()?),
        Listener::Unix(_) => None,
    };
    let origin = match (&settings.public_url, local_addr) {
        (Some(url), _) => OriginPolicy::from_public_url(url),
        (None, Some(addr)) => OriginPolicy::loopback(addr.port()),
        (None, None) => bail!("public_url is required on a unix socket"),
    };

    let (auth, policy) = match &settings.auth {
        AuthSettings::DevToken => (
            AuthRuntime::DevToken(DevToken::generate(&settings.state_dir)?),
            AuthPolicy {
                mode: AuthMode::DevToken,
                allowed_subjects: Vec::new(),
                allowed_groups: Vec::new(),
            },
        ),
        AuthSettings::Oidc(oidc) => {
            let client_id = match &oidc.client_id {
                ClientIdSource::Value(id) => id.clone(),
                ClientIdSource::File(path) => read_text_secret(path)?,
            };
            let secret = read_text_secret(&oidc.client_secret_file)?;
            let policy = AuthPolicy {
                mode: AuthMode::Oidc,
                allowed_subjects: oidc.allowed_subjects.clone(),
                allowed_groups: oidc.allowed_groups.clone(),
            };
            let oidc = Oidc::new(oidc.clone(), client_id, secret, &origin.public_origin)?;
            (AuthRuntime::Oidc(Box::new(oidc)), policy)
        }
    };
    let dev_login_url = match &auth {
        AuthRuntime::DevToken(dev) => Some(format!(
            "{}/auth/dev-token?token={}",
            origin.public_origin,
            dev.token()
        )),
        AuthRuntime::Oidc(_) => None,
    };

    let push = match &settings.push {
        Some(push) => {
            let subject = match (&push.subject, origin.secure) {
                (Some(subject), _) => subject.clone(),
                (None, true) => origin.public_origin.clone(),
                (None, false) => bail!(
                    "push.subject (a mailto: or https: contact) is required without an https \
                     public_url"
                ),
            };
            Some(PushConfig::web_push(&push.vapid_key_file, subject)?)
        }
        None => None,
    };

    let host = url::Url::parse(&origin.public_origin)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| "localhost".into());
    let controller = Controller::start(ControllerConfig {
        store_path: settings.state_dir.join(STORE_FILE),
        master_key,
        bundled_daemons_dir: settings.bundled_daemons_dir.clone(),
        server_version: VERSION.to_owned(),
        auth: policy,
        push,
        key_comment: controlroom_client_core::keys::default_comment(&host),
        agentd_socket: Some(settings.agentd_socket.clone()),
        tuning: Tuning {
            auth_session_ttl: settings.session_ttl,
            ..Tuning::default()
        },
    })?;

    let state = Arc::new(AppState {
        controller: controller.clone(),
        csp: HeaderValue::from_str(&Csp::app(&origin).render())
            .context("rendering the CSP header")?,
        cookies: Cookies::new(origin.secure),
        session_ttl: settings.session_ttl,
        origin,
        auth,
        assets: options.assets,
    });
    let public_origin = state.origin.public_origin.clone();
    let app = router(state);
    let (shutdown, shutdown_rx) = oneshot::channel::<()>();
    let graceful = async move {
        let _ = shutdown_rx.await;
    };
    let serve = match listener {
        Listener::Tcp(l) => {
            tokio::spawn(async move { axum::serve(l, app).with_graceful_shutdown(graceful).await })
        }
        Listener::Unix(l) => {
            tokio::spawn(async move { axum::serve(l, app).with_graceful_shutdown(graceful).await })
        }
    };
    let purge = tokio::spawn(purge_logins(controller.clone()));
    tracing::info!(listen = %settings.listen, %public_origin, "controlroom-web started");
    Ok(Server {
        local_addr,
        public_origin,
        dev_login_url,
        controller,
        shutdown,
        serve,
        purge,
    })
}

async fn purge_logins(controller: Controller) {
    let mut interval = tokio::time::interval(PURGE_INTERVAL);
    loop {
        interval.tick().await;
        match controller.purge_expired_logins() {
            Ok(0) => {}
            Ok(n) => tracing::info!(n, "deleted expired login sessions"),
            Err(e) => tracing::warn!(error = %e, "deleting expired login sessions"),
        }
    }
}

/// The routes. Middleware, outermost first: security headers, the Host allowlist, CSRF.
fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok\n" }))
        .route("/auth/session", get(auth::session_info))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/login", get(oidc::login))
        .route("/auth/callback", get(oidc::callback))
        .route("/auth/dev-token", get(dev_token::exchange))
        .route("/api/ws", get(ws::upgrade))
        .route(
            "/api/stream/terminal/{session}/{shell}",
            get(terminal::upgrade),
        )
        // Reserved (spec §19, §23): file transfer, served with `nosniff` and
        // `Content-Security-Policy: sandbox` from a separate content origin, and other byte
        // streams.
        .route("/api/files/{*rest}", any(static_files::reserved))
        .route("/api/stream/{*rest}", any(static_files::reserved))
        .fallback(static_files::serve)
        .layer(from_fn_with_state(state.clone(), hardening::csrf_guard))
        .layer(from_fn_with_state(state.clone(), hardening::host_allowlist))
        .layer(from_fn_with_state(
            state.clone(),
            hardening::security_headers,
        ))
        .with_state(state)
}
