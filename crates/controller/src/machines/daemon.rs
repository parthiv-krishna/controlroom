//! Opening the daemon link over an SSH connection: `controlroomd connect` on an exec channel,
//! the controller `hello`, and the post-connect steps (machine feed, session subscriptions,
//! doctor).
//!
//! **Two daemon generations.** The host daemon (`controlroomd` in the host role, host API)
//! answers `hello` with an `epoch`; the deprecated per-machine daemon (daemon API, sessions on
//! the machine) without one. Both share the `hello` params, so one handshake tells them apart:
//! a host link carries the agent daemon's host requests, a legacy link serves the old UI API
//! until R2 removes it.

use std::sync::{Arc, OnceLock};

use controlroom_client_core::bootstrap::daemon_command;
use controlroom_client_core::daemon::{
    CallOptions, CallTimeout, ClientOptions, DaemonClient, HelloError, check_version, hello_value,
};
use controlroom_client_core::ssh::SshConnection;
use controlroom_protocol::daemon::methods::{DoctorReportMethod, MachineSubscribe};
use controlroom_protocol::daemon::{HelloResult, MachineSubscribeParams, MachineSubscribeResult};
use controlroom_protocol::host::HostHelloResult;
use controlroom_protocol::ids::{DaemonId, MachineId};
use controlroom_protocol::version::Capability;
use controlroom_protocol::{
    DAEMON_PROTOCOL_VERSION, ErrorData, HOST_PROTOCOL_VERSION, NoParams, RpcError, RpcOutcome,
};
use serde_json::Value;

use crate::hub::{Hub, LinkKind};

/// What the daemon said in `hello`.
#[derive(Debug, Clone)]
pub(crate) enum Hello {
    /// A host daemon.
    Host(HostHelloResult),
    /// A legacy per-machine daemon.
    Legacy(HelloResult),
}

impl Hello {
    /// The daemon's software version.
    pub(crate) fn version(&self) -> &str {
        match self {
            Hello::Host(h) => &h.host_version,
            Hello::Legacy(h) => &h.daemon_version,
        }
    }

    pub(crate) fn daemon_id(&self) -> DaemonId {
        match self {
            Hello::Host(h) => h.daemon_id,
            Hello::Legacy(h) => h.daemon_id,
        }
    }

    pub(crate) fn capabilities(&self) -> &[Capability] {
        match self {
            Hello::Host(h) => &h.capabilities,
            Hello::Legacy(h) => &h.capabilities,
        }
    }

    pub(crate) fn kind(&self) -> LinkKind {
        match self {
            Hello::Host(h) => LinkKind::Host { epoch: h.epoch },
            Hello::Legacy(_) => LinkKind::Legacy,
        }
    }

    /// Tells the generations apart by the result's shape and checks the protocol major.
    fn parse(value: Value) -> Result<Self, HelloError> {
        let malformed = |e: serde_json::Error| {
            HelloError::Call(controlroom_client_core::daemon::CallError::Rpc(
                RpcError::new(
                    ErrorData::InvalidResponse,
                    format!("malformed hello result: {e}"),
                ),
            ))
        };
        if value.get("epoch").is_some() {
            let h: HostHelloResult = serde_json::from_value(value).map_err(malformed)?;
            check_version(HOST_PROTOCOL_VERSION, &h.host_version, h.protocol_version)?;
            Ok(Hello::Host(h))
        } else {
            let h: HelloResult = serde_json::from_value(value).map_err(malformed)?;
            check_version(
                DAEMON_PROTOCOL_VERSION,
                &h.daemon_version,
                h.protocol_version,
            )?;
            Ok(Hello::Legacy(h))
        }
    }
}

/// A handshaken daemon connection.
pub(crate) struct DaemonLink {
    pub client: DaemonClient,
    pub hello: Hello,
}

/// Why the daemon link couldn't be opened.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OpenError {
    /// `controlroomd connect` couldn't be started.
    #[error("starting controlroomd connect: {0}")]
    Exec(String),
    /// The handshake failed or was refused.
    #[error("{error}{}", stderr_suffix(.stderr))]
    Hello {
        error: HelloError,
        /// What `controlroomd connect` printed, for diagnostics.
        stderr: String,
    },
}

fn stderr_suffix(stderr: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        String::new()
    } else {
        format!(" ({stderr})")
    }
}

/// Runs `controlroomd connect` and says `hello` as this controller. Notifications go to the
/// hub, routed by the link's kind once `hello` told it.
pub(crate) async fn open(
    hub: &Arc<Hub>,
    id: MachineId,
    conn: &SshConnection,
) -> Result<DaemonLink, OpenError> {
    let channel = conn
        .exec(&daemon_command(&["connect"]))
        .await
        .map_err(|e| OpenError::Exec(e.to_string()))?;
    let (reader, writer, exit) = channel.into_parts();
    let weak = Arc::downgrade(hub);
    let kind: Arc<OnceLock<LinkKind>> = Arc::default();
    let route = kind.clone();
    let client = DaemonClient::start(
        reader,
        writer,
        ClientOptions {
            call_timeout: hub.services.tuning.call_timeout,
            ..ClientOptions::default()
        },
        Box::new(move |n| {
            let Some(hub) = weak.upgrade() else { return };
            match route.get() {
                Some(LinkKind::Host { .. }) => hub.on_host_notification(id, n),
                Some(LinkKind::Legacy) => hub.on_daemon_notification(id, n),
                None => {
                    tracing::debug!(machine = %id, method = %n.method, "notification before hello")
                }
            }
        }),
    );
    let hello = hello_value(
        &client,
        HOST_PROTOCOL_VERSION,
        &hub.services.client_name(),
        hub.services.controller_id,
    )
    .await
    .and_then(Hello::parse);
    match hello {
        Ok(hello) => {
            let _ = kind.set(hello.kind());
            Ok(DaemonLink { client, hello })
        }
        Err(error) => {
            client.close();
            Err(OpenError::Hello {
                error,
                stderr: String::from_utf8_lossy(&exit.stderr()).into_owned(),
            })
        }
    }
}

/// Subscribes to the machine feed from the last revision applied. The response hook applies
/// the result before any `machine.events` of the subscription are routed.
pub(crate) async fn subscribe_machine(
    hub: &Arc<Hub>,
    id: MachineId,
    client: &DaemonClient,
) -> Result<(), String> {
    let hook_hub = hub.clone();
    let options = CallOptions {
        on_response: Some(Box::new(move |response| {
            if let RpcOutcome::Result(value) = &response.outcome {
                match serde_json::from_value::<MachineSubscribeResult>(value.clone()) {
                    Ok(result) => hook_hub.machine_subscribed(id, result),
                    Err(e) => {
                        tracing::warn!(machine = %id, error = %e, "bad machine.subscribe result")
                    }
                }
            }
        })),
        ..CallOptions::default()
    };
    let params = MachineSubscribeParams {
        since_rev: hub.since_rev(id),
    };
    client
        .call_with::<MachineSubscribe>(&params, options)
        .await
        .map(drop)
        .map_err(|e| format!("machine.subscribe failed: {e}"))
}

/// Runs the doctor (the same method on both daemon generations) and stores the report; the
/// caller aborts it when the link ends.
pub(crate) fn spawn_doctor(
    hub: &Arc<Hub>,
    id: MachineId,
    client: &DaemonClient,
) -> tokio::task::JoinHandle<()> {
    let (hub, client) = (hub.clone(), client.clone());
    let options = CallOptions {
        timeout: CallTimeout::After(hub.services.tuning.slow_call_timeout),
        ..CallOptions::default()
    };
    tokio::spawn(async move {
        match client
            .call_with::<DoctorReportMethod>(&NoParams {}, options)
            .await
        {
            Ok(report) => hub.set_doctor(id, &report),
            Err(e) => tracing::warn!(machine = %id, error = %e, "doctor.report failed"),
        }
    })
}
