//! The [`Controller`] handle the web shell drives.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use controlroom_client_core::bootstrap::DirBinaries;
use controlroom_protocol::ids::{DeviceId, SessionId};
use controlroom_protocol::ui::AuthMode;
use controlroom_protocol::{RpcError, UnixMillis};

use crate::auth::{
    AuthError, Authenticated, Identity, NewLogin, Principal, random_token, token_hash,
};
use crate::config::{ControllerConfig, Services};
use crate::errors::{invalid_params, not_found, store_error};
use crate::hub::{CloseReason, Hub};
use crate::notify::parse_subscription;
use crate::rpc::{self, UiClient};
use crate::store::{NewAuthSession, OwnerId, Store, StoreError};
use crate::terminals::{self, Opener, TerminalStream};

/// Why the controller couldn't start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The client store couldn't be opened.
    #[error("client store: {0}")]
    Store(#[from] StoreError),
    /// An owner's hub couldn't start.
    #[error("starting owner {owner}: {error}")]
    Hub {
        /// The owner.
        owner: OwnerId,
        /// Why.
        error: RpcError,
    },
}

/// The controller: one per server process, shared by every connection. Cheap to clone.
#[derive(Clone)]
pub struct Controller {
    services: Arc<Services>,
    hubs: Arc<Mutex<HashMap<OwnerId, Arc<Hub>>>>,
}

impl Controller {
    /// Opens the store, generates each owner's controlroom key on first start, and starts
    /// every machine. Must run inside a Tokio runtime. Without usable bundled daemons the
    /// controller still starts (machines with a daemon connect; nothing is installed).
    pub fn start(config: ControllerConfig) -> Result<Self, StartError> {
        let store = Store::open(&config.store_path)?;
        let controller_id = store.controller_id()?;
        let binaries = match &config.bundled_daemons_dir {
            None => Err("no bundled daemons directory is configured".to_owned()),
            Some(dir) => DirBinaries::open(dir)
                .map(Arc::new)
                .map_err(|e| format!("bundled daemons: {e}")),
        };
        if let Err(why) = &binaries {
            tracing::warn!(%why, "daemons can't be installed or upgraded");
        }
        let services = Arc::new(Services {
            store,
            master_key: config.master_key,
            binaries,
            server_version: config.server_version,
            controller_id,
            auth: config.auth,
            push: config.push,
            key_comment: config.key_comment,
            agentd_socket: config.agentd_socket,
            tuning: config.tuning,
        });
        let controller = Self {
            services,
            hubs: Arc::default(),
        };
        let mut owners = controller.services.store.owner_ids()?;
        if owners.is_empty() {
            owners.push(OwnerId::local());
        }
        for owner in owners {
            controller
                .hub(&owner)
                .map_err(|error| StartError::Hub { owner, error })?;
        }
        Ok(controller)
    }

    /// The owner's hub, started on first use.
    fn hub(&self, owner: &OwnerId) -> Result<Arc<Hub>, RpcError> {
        let mut hubs = self.hubs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(hub) = hubs.get(owner) {
            return Ok(hub.clone());
        }
        let hub = Hub::start(self.services.clone(), owner.clone())?;
        hubs.insert(owner.clone(), hub.clone());
        Ok(hub)
    }

    /// The deployment's auth mode.
    pub fn auth_mode(&self) -> AuthMode {
        self.services.auth.mode
    }

    /// The VAPID public key, when Web Push is configured.
    pub fn vapid_public_key(&self) -> Option<&str> {
        self.services
            .push
            .as_ref()
            .map(|p| p.vapid_public_key.as_str())
    }

    // --- Authentication ---------------------------------------------------------------------

    /// Admits an identity established by the shell (OIDC callback or dev token).
    pub fn resolve(&self, identity: &Identity) -> Result<Principal, AuthError> {
        self.services.auth.resolve(identity)
    }

    /// Creates a login session for an admitted principal; the token goes into the cookie.
    pub fn create_login(
        &self,
        principal: &Principal,
        device: Option<DeviceId>,
    ) -> Result<NewLogin, AuthError> {
        self.services.auth.check(principal)?;
        let token = random_token();
        let csrf_token = random_token();
        let expires_at = self.expiry(UnixMillis::now());
        self.services.store.ensure_owner(&principal.owner)?;
        self.services
            .store
            .owner(&principal.owner)
            .auth_sessions()
            .create(NewAuthSession {
                token_hash: token_hash(&token),
                mode: principal.mode,
                subject: principal.subject.clone(),
                groups: principal.groups.clone(),
                name: principal.name.clone(),
                email: principal.email.clone(),
                device_id: device,
                csrf_token: csrf_token.clone(),
                expires_at,
            })?;
        Ok(NewLogin {
            token,
            csrf_token,
            expires_at,
        })
    }

    fn expiry(&self, now: UnixMillis) -> UnixMillis {
        let ttl = self.services.tuning.auth_session_ttl.as_millis();
        UnixMillis(now.0.saturating_add(i64::try_from(ttl).unwrap_or(i64::MAX)))
    }

    /// Resolves a cookie token: the live login session, its expiry slid forward, re-checked
    /// against the auth mode and allow-list. Call it on every HTTP request.
    pub fn authenticate(&self, token: &str) -> Result<Authenticated, AuthError> {
        let now = UnixMillis::now();
        let hash = token_hash(token);
        let row = self
            .services
            .store
            .auth_sessions()
            .touch(&hash, now, self.expiry(now))?
            .ok_or(AuthError::NoSession)?;
        let principal = Principal::from_session(&row);
        self.services.auth.check(&principal)?;
        Ok(Authenticated {
            principal,
            token_hash: hash,
            csrf_token: row.csrf_token,
            device_id: row.device_id,
        })
    }

    /// Logout: revokes the login session and closes its open connections.
    pub fn logout(&self, auth: &Authenticated) -> Result<(), AuthError> {
        self.services
            .store
            .owner(&auth.principal.owner)
            .auth_sessions()
            .revoke(&auth.token_hash)?;
        if let Ok(hub) = self.hub(&auth.principal.owner) {
            hub.close_clients(CloseReason::Unauthenticated, |c| {
                c.auth_token_hash.as_deref() == Some(auth.token_hash.as_str())
            });
        }
        Ok(())
    }

    /// Removes a device: revokes its login sessions, closes its connections and drops its push
    /// subscription (the same as `devices.remove`).
    pub fn revoke_device(&self, owner: &OwnerId, device: &DeviceId) -> Result<(), RpcError> {
        remove_device(&*self.hub(owner)?, device)
    }

    /// Signs the owner out everywhere: every login session and connection.
    pub fn revoke_owner(&self, owner: &OwnerId) -> Result<(), RpcError> {
        let hub = self.hub(owner)?;
        hub.repo()
            .auth_sessions()
            .revoke_all()
            .map_err(store_error)?;
        hub.close_clients(CloseReason::Unauthenticated, |_| true);
        Ok(())
    }

    /// Deletes expired login sessions; returns how many. Run it now and then.
    pub fn purge_expired_logins(&self) -> Result<usize, StoreError> {
        self.services
            .store
            .auth_sessions()
            .purge_expired(UnixMillis::now())
    }

    // --- UI connections ---------------------------------------------------------------------

    /// Opens a UI connection for an authenticated login session. Every request on it
    /// re-checks the session and the allow-list; revoking the session closes it.
    pub fn open_client(&self, auth: &Authenticated) -> Result<UiClient, RpcError> {
        let hub = self.hub(&auth.principal.owner)?;
        Ok(rpc::open(
            hub,
            auth.principal.clone(),
            Some(auth.token_hash.clone()),
        ))
    }

    /// Opens a UI connection for a principal without a login session (an embedding that
    /// authenticates otherwise, and tests). The allow-list is still re-checked per request.
    pub fn open_trusted_client(&self, principal: Principal) -> Result<UiClient, RpcError> {
        let hub = self.hub(&principal.owner)?;
        Ok(rpc::open(hub, principal, None))
    }

    /// Opens a terminal stream to `shell` of `session` for an authenticated login session
    /// (spec §22): `unknown_session` or `shell_gone` when there is no such shell. Revoking the
    /// login closes it; see [`TerminalStream`].
    pub fn open_terminal(
        &self,
        auth: &Authenticated,
        session: SessionId,
        shell: &str,
    ) -> Result<TerminalStream, RpcError> {
        let hub = self.hub(&auth.principal.owner)?;
        let opener = Opener {
            principal: auth.principal.clone(),
            token_hash: Some(auth.token_hash.clone()),
            device: auth.device_id.clone(),
        };
        terminals::open(hub, opener, session, shell)
    }

    /// Attaches a browser's `PushSubscription.toJSON()` to a registered device (the same as
    /// `notifications.subscribe`).
    pub fn subscribe_push(
        &self,
        owner: &OwnerId,
        device: &DeviceId,
        subscription: &serde_json::Value,
    ) -> Result<(), RpcError> {
        let hub = self.hub(owner)?;
        let row = parse_subscription(device, subscription)?;
        if !hub.repo().push().set(&row).map_err(store_error)? {
            return Err(not_found(
                controlroom_protocol::error::Resource::Device,
                device,
            ));
        }
        hub.device_changed(device);
        Ok(())
    }

    /// Closes every connection and stops every machine task.
    pub async fn shutdown(&self) {
        let hubs: Vec<_> = self
            .hubs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
            .map(|(_, hub)| hub)
            .collect();
        for hub in hubs {
            hub.shutdown().await;
        }
    }
}

/// `devices.remove` and [`Controller::revoke_device`].
pub(crate) fn remove_device(hub: &Hub, device: &DeviceId) -> Result<(), RpcError> {
    if device.as_str().is_empty() {
        return Err(invalid_params("the device id is empty"));
    }
    {
        let mut st = hub.lock();
        if !hub.repo().devices().remove(device).map_err(store_error)? {
            return Err(not_found(
                controlroom_protocol::error::Resource::Device,
                device,
            ));
        }
        st.device_removed(device);
    }
    hub.close_clients(CloseReason::Unauthenticated, |c| {
        c.device.as_ref() == Some(device)
    });
    Ok(())
}
