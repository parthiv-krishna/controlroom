//! Opening the daemon link over an SSH connection: `controlroomd connect` on an exec channel,
//! the controller `hello`, and the post-connect steps (machine feed, session subscriptions,
//! doctor).

use std::sync::Arc;

use controlroom_client_core::bootstrap::daemon_command;
use controlroom_client_core::daemon::{
    CallOptions, ClientOptions, DaemonClient, HelloError, hello,
};
use controlroom_client_core::ssh::SshConnection;
use controlroom_protocol::NoParams;
use controlroom_protocol::RpcOutcome;
use controlroom_protocol::daemon::methods::{DoctorReportMethod, MachineSubscribe};
use controlroom_protocol::daemon::{HelloResult, MachineSubscribeParams, MachineSubscribeResult};
use controlroom_protocol::ids::MachineId;

use crate::hub::Hub;

/// A handshaken daemon connection.
pub(crate) struct DaemonLink {
    pub client: DaemonClient,
    pub hello: HelloResult,
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
/// hub.
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
    let client = DaemonClient::start(
        reader,
        writer,
        ClientOptions {
            call_timeout: hub.services.tuning.call_timeout,
            ..ClientOptions::default()
        },
        Box::new(move |n| {
            if let Some(hub) = weak.upgrade() {
                hub.on_daemon_notification(id, n);
            }
        }),
    );
    match hello(
        &client,
        &hub.services.client_name(),
        hub.services.controller_id,
    )
    .await
    {
        Ok(hello) => Ok(DaemonLink { client, hello }),
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

/// Runs the doctor and stores the report; the caller aborts it when the link ends.
pub(crate) fn spawn_doctor(
    hub: &Arc<Hub>,
    id: MachineId,
    client: &DaemonClient,
) -> tokio::task::JoinHandle<()> {
    let (hub, client) = (hub.clone(), client.clone());
    let options = CallOptions {
        timeout: Some(hub.services.tuning.slow_call_timeout),
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
