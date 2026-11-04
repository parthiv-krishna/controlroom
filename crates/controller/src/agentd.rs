//! The agent daemon client: one connection to `controlroom-agentd`'s local socket, kept up with
//! backoff.
//!
//! After each `hello` (as this controller) it subscribes to every session's summary
//! (`sessions.subscribe` from the last applied revision), takes over the pending host requests
//! (`hostops.subscribe`), reports the machines (`hostops.machines`) and resumes every open
//! session subscription of the next UI API. Response hooks apply each subscription's result
//! before any of its notifications are routed. Disconnecting does nothing on the agent daemon's
//! side: its sessions keep running, and host calls only pause.

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use controlroom_client_core::daemon::{
    CallOptions, ClientOptions, DaemonClient, ResponseHook, agentd_hello,
};
use controlroom_protocol::agentd::methods::{HostOpsSubscribe, SessionsSubscribe};
use controlroom_protocol::agentd::{
    AgentdNotification, HostOpsSubscribeResult, SessionsSubscribeParams, SessionsSubscribeResult,
};
use controlroom_protocol::{NoParams, RpcNotification, RpcOutcome, RpcResponse};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::net::UnixStream;

use crate::hub::Hub;

/// Keeps the connection up until the hub is gone (the hub aborts the task on shutdown).
pub(crate) async fn run(hub: Weak<Hub>, socket: PathBuf) {
    let mut attempt: u32 = 0;
    loop {
        let Some(strong) = hub.upgrade() else { return };
        let backoff = strong.services.tuning.backoff;
        match connect(&strong, &socket).await {
            Ok(client) => {
                attempt = 0;
                drop(strong);
                let reason = client.closed().await;
                tracing::info!(%reason, "agent daemon connection lost");
                let Some(strong) = hub.upgrade() else { return };
                strong.agentd_disconnected(&client);
            }
            Err(why) => {
                tracing::warn!(socket = %socket.display(), %why, "agent daemon unavailable")
            }
        }
        tokio::time::sleep(backoff.delay(attempt)).await;
        attempt = attempt.saturating_add(1);
    }
}

/// Connects, says hello and runs the post-connect steps.
async fn connect(hub: &Arc<Hub>, socket: &std::path::Path) -> Result<DaemonClient, String> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| format!("connecting: {e}"))?;
    let (reader, writer) = stream.into_split();
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
                on_notification(&hub, n);
            }
        }),
    );
    let hello = agentd_hello(
        &client,
        &hub.services.client_name(),
        hub.services.controller_id,
    )
    .await
    .map_err(|e| {
        client.close();
        e.to_string()
    })?;
    tracing::info!(version = %hello.daemon_version, "connected to the agent daemon");
    hub.agentd_connected(client.clone());
    if let Err(e) = subscribe(hub, &client).await {
        client.close();
        hub.agentd_disconnected(&client);
        return Err(e);
    }
    crate::rpc::resubscribe_global(hub, &client).await;
    Ok(client)
}

/// A hook decoding a subscription result and applying it.
fn hook<T: DeserializeOwned + 'static>(
    hub: &Arc<Hub>,
    apply: impl FnOnce(&Arc<Hub>, T) + Send + 'static,
) -> ResponseHook {
    let hub = hub.clone();
    Box::new(move |response: &RpcResponse<Value>| {
        if let RpcOutcome::Result(value) = &response.outcome {
            match serde_json::from_value::<T>(value.clone()) {
                Ok(result) => apply(&hub, result),
                Err(e) => tracing::warn!(error = %e, "bad agent daemon subscription result"),
            }
        }
    })
}

async fn subscribe(hub: &Arc<Hub>, client: &DaemonClient) -> Result<(), String> {
    let since_rev = hub.lock().global.rev;
    let options = CallOptions {
        on_response: Some(hook(hub, |hub, r: SessionsSubscribeResult| {
            hub.lock().sessions_subscribed(r);
        })),
        ..CallOptions::default()
    };
    client
        .call_with::<SessionsSubscribe>(&SessionsSubscribeParams { since_rev }, options)
        .await
        .map_err(|e| format!("sessions.subscribe failed: {e}"))?;
    let options = CallOptions {
        on_response: Some(hook(hub, |hub, r: HostOpsSubscribeResult| {
            let mut st = hub.lock();
            hub.hostops_subscribed(&mut st, r.subscription_id, r.requests);
        })),
        ..CallOptions::default()
    };
    client
        .call_with::<HostOpsSubscribe>(&NoParams {}, options)
        .await
        .map_err(|e| format!("hostops.subscribe failed: {e}"))?;
    hub.report_machines(true);
    Ok(())
}

/// Routes an agent daemon notification (on the reader task).
fn on_notification(hub: &Arc<Hub>, n: RpcNotification<Value>) {
    match AgentdNotification::from_parts(&n.method, n.params) {
        Ok(AgentdNotification::SessionsEvents(p)) => hub.lock().sessions_events(p),
        Ok(AgentdNotification::SessionEvents(p)) => hub.global_timeline_events(&p),
        Ok(AgentdNotification::HostOpsRequest(p)) => {
            let mut st = hub.lock();
            hub.hostops_requested(&mut st, p.subscription_id, p.requests);
        }
        Ok(AgentdNotification::HostOpsWithdraw(p)) => {
            let mut st = hub.lock();
            hub.hostops_withdrawn(&mut st, p.subscription_id, &p.ids);
        }
        Err(e) => {
            tracing::warn!(method = %n.method, error = %e.message, "bad agent daemon notification")
        }
    }
}
