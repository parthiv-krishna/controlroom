//! Shared setup for the controller's integration tests.

#![allow(dead_code)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use controlroom_client_core::ssh::ConnectOptions;
use controlroom_controller::test_support::{RemoteHost, UiDriver};
use controlroom_controller::{
    AuthPolicy, Backoff, Controller, ControllerConfig, Identity, MasterKey, PushConfig,
    PushOutcome, PushSender, Tuning, UiApi,
};
use controlroom_protocol::ui::AuthMode;
use futures::future::BoxFuture;
use serde_json::{Value, json};

pub mod relay;

/// A controller's scratch directory and its bundled daemons.
pub struct Setup {
    pub controller: Controller,
    pub dir: tempfile::TempDir,
}

pub fn tuning() -> Tuning {
    Tuning {
        ssh: ConnectOptions {
            connect_timeout: Duration::from_secs(5),
            keepalive_interval: Duration::from_secs(5),
            keepalive_max: 3,
        },
        backoff: Backoff {
            initial: Duration::from_millis(100),
            max: Duration::from_millis(500),
        },
        auto_upgrade_quiet: Duration::from_millis(200),
        auto_upgrade_retry: Backoff {
            initial: Duration::from_secs(3600),
            max: Duration::from_secs(3600),
        },
        ..Tuning::default()
    }
}

pub fn config(
    store_path: &Path,
    bundled: Option<&Path>,
    push: Option<PushConfig>,
) -> ControllerConfig {
    ControllerConfig {
        store_path: store_path.to_owned(),
        master_key: MasterKey::from_bytes([7; 32]),
        bundled_daemons_dir: bundled.map(Path::to_owned),
        server_version: "0.0.0-test".into(),
        auth: AuthPolicy {
            mode: AuthMode::DevToken,
            allowed_subjects: vec![],
            allowed_groups: vec![],
        },
        push,
        key_comment: "controlroom@controlroom.example.com".into(),
        agentd_socket: None,
        tuning: tuning(),
    }
}

/// A controller on a fresh store.
pub fn controller(bundled: Option<&Path>, push: Option<PushConfig>) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let controller =
        Controller::start(config(&dir.path().join("controller.sqlite"), bundled, push)).unwrap();
    Setup { controller, dir }
}

/// A controller on a fresh store, linked to the agent daemon at `agentd`.
pub fn controller_with_agentd(bundled: Option<&Path>, agentd: &Path) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(&dir.path().join("controller.sqlite"), bundled, None);
    config.agentd_socket = Some(agentd.to_owned());
    let controller = Controller::start(config).unwrap();
    Setup { controller, dir }
}

/// A next-UI-API connection for the dev user, handshaken as `device`, with the state feed
/// mirrored.
pub async fn ui_next(controller: &Controller, device: &str) -> UiDriver {
    let principal = controller.resolve(&Identity::DevToken).unwrap();
    let client = controller
        .open_trusted_client_with(principal, UiApi::Next)
        .unwrap();
    let mut ui = UiDriver::new(client);
    ui.hello(device, &format!("{device} (Firefox on Linux)"))
        .await;
    ui.subscribe_state().await;
    ui
}

/// A UI connection for the dev user, handshaken as `device`, with the state feed mirrored.
pub async fn ui(controller: &Controller, device: &str) -> UiDriver {
    let principal = controller.resolve(&Identity::DevToken).unwrap();
    let mut ui = UiDriver::new(controller.open_trusted_client(principal).unwrap());
    ui.hello(device, &format!("{device} (Firefox on Linux)"))
        .await;
    ui.subscribe_state().await;
    ui
}

/// The controlroom key's public line.
pub async fn controlroom_key(ui: &mut UiDriver) -> String {
    ui.ok("keys.list", json!({})).await["controlroom"]["public_key"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Adds `host` (authorizing the controlroom key first); returns the machine id once it is
/// connected.
pub async fn add_machine(ui: &mut UiDriver, host: &RemoteHost, name: &str) -> String {
    add_machine_at(ui, host, name, host.port()).await
}

/// Like [`add_machine`], reaching the host's SSH server on `port` (e.g. through a [`relay::Relay`]).
pub async fn add_machine_at(ui: &mut UiDriver, host: &RemoteHost, name: &str, port: u16) -> String {
    let key = controlroom_key(ui).await;
    host.authorize(&key);
    let mut new = host.new_machine(name);
    new["port"] = json!(port);
    let added = ui.ok("machines.add", new.clone()).await;
    let mut confirm = new;
    confirm["host_key_fingerprint"] = added["host_key"]["fingerprint"].clone();
    let result = ui.ok("machines.add_confirm", confirm).await;
    assert_eq!(result["outcome"], "added", "{result}");
    let id = result["machine"]["id"].as_str().unwrap().to_owned();
    ui.wait("the machine connects", |m| {
        m.machine(&id)["connectivity"]["state"] == "connected"
    })
    .await;
    id
}

/// Creates a session (sandbox off: tests run without nono) and waits until it is idle.
pub async fn create_session(
    ui: &mut UiDriver,
    machine: &str,
    host: &RemoteHost,
    extra: Value,
) -> String {
    let mut params = json!({"machine_id": machine, "op_id": controlroom_controller::test_support::op(),
        "agent": "fake", "cwd": host.work(), "sandbox_level": "off"});
    for (k, v) in extra.as_object().into_iter().flatten() {
        params[k] = v.clone();
    }
    let created = ui.ok("session.create", params).await;
    created["session"]["id"].as_str().unwrap().to_owned()
}

/// Records every push.
#[derive(Default)]
pub struct RecordingPush {
    pub sent: Mutex<Vec<(String, Value)>>,
}

impl PushSender for RecordingPush {
    fn send<'a>(
        &'a self,
        sub: &'a controlroom_controller::store::PushSubscriptionRow,
        payload: &'a [u8],
        _urgent: bool,
    ) -> BoxFuture<'a, PushOutcome> {
        let payload: Value = serde_json::from_slice(payload).unwrap();
        self.sent
            .lock()
            .unwrap()
            .push((sub.device_id.to_string(), payload));
        Box::pin(async { PushOutcome::Delivered })
    }
}

impl RecordingPush {
    pub fn config(self: &Arc<Self>) -> PushConfig {
        PushConfig {
            vapid_public_key: "BTest".into(),
            sender: self.clone(),
        }
    }

    pub fn sent(&self) -> Vec<(String, Value)> {
        self.sent.lock().unwrap().clone()
    }
}

/// A push subscription JSON as a browser would send it.
pub fn browser_subscription(n: u32) -> Value {
    json!({"endpoint": format!("https://push.example.com/send/{n}"),
           "keys": {"p256dh": "BPubKey", "auth": "AuthSecret"}})
}
