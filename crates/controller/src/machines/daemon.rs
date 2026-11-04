//! Opening the host daemon link over an SSH connection: `controlroomd connect` on an exec
//! channel and the controller `hello` (host API), then the doctor. The link carries the agent
//! daemon's host requests (`crate::router`); its notifications are forwarded to the agent
//! daemon.

use std::sync::Arc;

use controlroom_client_core::bootstrap::daemon_command;
use controlroom_client_core::daemon::{
    CallOptions, CallTimeout, ClientOptions, DaemonClient, HelloError, host_hello,
};
use controlroom_client_core::ssh::SshConnection;
use controlroom_protocol::NoParams;
use controlroom_protocol::host::HostHelloResult;
use controlroom_protocol::host::methods::DoctorReportMethod;
use controlroom_protocol::ids::MachineId;

use crate::hub::Hub;

/// A handshaken host daemon connection.
pub(crate) struct DaemonLink {
    pub client: DaemonClient,
    pub hello: HostHelloResult,
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
/// hub, which forwards them to the agent daemon.
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
                hub.on_host_notification(id, n);
            }
        }),
    );
    let hello = host_hello(
        &client,
        &hub.services.client_name(),
        hub.services.controller_id,
    )
    .await;
    match hello {
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

/// Runs the host's doctor and stores the report; the caller aborts it when the link ends.
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
