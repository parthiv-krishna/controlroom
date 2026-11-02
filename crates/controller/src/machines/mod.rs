//! The machine manager: one supervising task per machine.
//!
//! Each task connects over russh with the pinned host key and the machine's key, bootstraps
//! the daemon (probe, install when missing), opens `controlroomd connect`, says `hello` as this
//! controller, subscribes, and keeps reconnecting with backoff. Host-key, authentication and
//! controller-conflict problems stop it in `action_required` until the user acts; an
//! incompatible daemon waits for an explicit upgrade. Tasks share nothing that blocks: an
//! unreachable machine never holds up another.
//!
//! **Automatic upgrades** (spec §2): while connected to an older, compatible daemon with the
//! machine's `auto_upgrade` on, the task waits until no run is in flight and the machine has
//! been quiet for a while, then upgrades (verify-run, stop, `hello` check, rollback on
//! failure). A failed attempt leaves the machine on the old daemon with `last_error` set and
//! retries after a backoff. Daemons are never downgraded.
//!
//! The daemon outbox (mailbox broker) is consumed from P7 on; nothing subscribes to it yet.

mod bootstrap;
pub(crate) mod connect;
mod daemon;

use std::sync::Arc;

use controlroom_client_core::daemon::HelloError;
use controlroom_client_core::ssh::SshConnection;
use controlroom_protocol::ids::{KeyId, MachineId};
use controlroom_protocol::ui::{ActionRequiredReason, BootstrapStep, Connectivity};
use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;

pub(crate) use bootstrap::bundled_is_newer;
use daemon::{DaemonLink, OpenError};

use crate::hub::{BusEvent, Hub, Link, UpgradeCheck};
use crate::store::SeenUpdate;

/// User actions for a machine task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MachineCommand {
    /// Reconnect now (also retries after `action_required`).
    Reconnect,
    /// Upgrade to the bundled daemon now (also from `incompatible`).
    Upgrade,
    /// End the task.
    Stop,
}

/// When the task upgrades the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallPolicy {
    /// Only install a missing daemon (ordinary reconnects).
    IfMissing,
    /// Also upgrade an older one: right after adding the machine, and on `machines.upgrade`.
    IfOlder,
}

/// How one SSH connection's life ended.
enum End {
    Stop,
    Reconnect,
    /// An established link was lost; reconnect promptly.
    Lost(String),
    /// Bootstrap or the handshake failed; retry with backoff.
    Failed(String),
    /// The user must act first.
    Action(Connectivity, String),
}

/// Automatic-upgrade bookkeeping; it outlives connections.
#[derive(Debug, Default)]
struct AutoUpgrade {
    /// An automatic upgrade is running; its outcome is judged at the next handshake.
    attempting: bool,
    failures: u32,
    retry_at: Option<Instant>,
    /// Why the last attempt failed; shown while the old daemon stays in use.
    error: Option<String>,
}

impl AutoUpgrade {
    fn failed(&mut self, hub: &Hub, id: MachineId, reason: &str) {
        self.attempting = false;
        let delay = hub.services.tuning.auto_upgrade_retry.delay(self.failures);
        self.failures = self.failures.saturating_add(1);
        self.retry_at = Some(Instant::now() + delay);
        tracing::warn!(machine = %id, %reason, retry_in = ?delay, "automatic daemon upgrade failed");
        self.error = Some(format!("automatic upgrade failed: {reason}"));
    }
}

/// The task body.
pub(crate) async fn run(
    hub: Arc<Hub>,
    id: MachineId,
    mut rx: mpsc::Receiver<MachineCommand>,
    mut initial: Option<(SshConnection, KeyId)>,
    mut policy: InstallPolicy,
) {
    let mut attempt: u32 = 0;
    let mut auto = AutoUpgrade::default();
    loop {
        let connected = match initial.take() {
            Some(conn) => Ok(conn),
            None => {
                hub.update_live(id, |l| {
                    l.connectivity = Connectivity::Connecting;
                    l.bootstrap = None;
                });
                let plan = match connect::plan(&hub, id) {
                    Ok(plan) => plan,
                    Err(e) => {
                        hub.disconnected(id, Connectivity::Unreachable, Some(e.message));
                        match rx.recv().await {
                            Some(MachineCommand::Stop) | None => return,
                            Some(MachineCommand::Upgrade) => policy = InstallPolicy::IfOlder,
                            Some(MachineCommand::Reconnect) => {}
                        }
                        continue;
                    }
                };
                tokio::select! {
                    r = connect::connect_any(&plan, &hub.services.tuning.ssh) => r,
                    cmd = rx.recv() => match cmd {
                        Some(MachineCommand::Stop) | None => return,
                        Some(MachineCommand::Upgrade) => { policy = InstallPolicy::IfOlder; continue }
                        Some(MachineCommand::Reconnect) => continue,
                    }
                }
            }
        };
        let end = match connected {
            Ok((conn, key_id)) => {
                let conn = Arc::new(conn);
                let end = session(&hub, id, &conn, key_id, &mut rx, &mut policy, &mut auto).await;
                conn.disconnect().await;
                end
            }
            Err(e) => match connect::failure_connectivity(&e) {
                c @ Connectivity::ActionRequired { .. } => End::Action(c, e.to_string()),
                _ => End::Failed(e.to_string()),
            },
        };
        match end {
            End::Stop => {
                hub.disconnected(id, Connectivity::Unreachable, None);
                return;
            }
            End::Reconnect => {
                attempt = 0;
                hub.disconnected(id, Connectivity::Connecting, None);
            }
            End::Lost(reason) => {
                tracing::info!(machine = %id, %reason, "machine connection lost");
                hub.disconnected(id, Connectivity::Connecting, Some(reason));
                // It worked until now: start the backoff over.
                attempt = 0;
                if !backoff(&hub, &mut rx, &mut attempt, &mut policy).await {
                    return;
                }
            }
            End::Failed(reason) => {
                tracing::warn!(machine = %id, %reason, "machine connection failed");
                hub.disconnected(id, Connectivity::Unreachable, Some(reason));
                if !backoff(&hub, &mut rx, &mut attempt, &mut policy).await {
                    return;
                }
            }
            End::Action(connectivity, reason) => {
                tracing::warn!(machine = %id, %reason, "machine needs user action");
                hub.disconnected(id, connectivity, Some(reason));
                match rx.recv().await {
                    Some(MachineCommand::Stop) | None => return,
                    Some(MachineCommand::Upgrade) => policy = InstallPolicy::IfOlder,
                    Some(MachineCommand::Reconnect) => {}
                }
                attempt = 0;
            }
        }
    }
}

/// Sleeps for the next backoff delay; a command cuts it short. Returns `false` on stop.
async fn backoff(
    hub: &Hub,
    rx: &mut mpsc::Receiver<MachineCommand>,
    attempt: &mut u32,
    policy: &mut InstallPolicy,
) -> bool {
    let delay = hub.services.tuning.backoff.delay(*attempt);
    *attempt = attempt.saturating_add(1);
    tokio::select! {
        () = tokio::time::sleep(delay) => true,
        cmd = rx.recv() => match cmd {
            Some(MachineCommand::Stop) | None => false,
            Some(MachineCommand::Upgrade) => { *policy = InstallPolicy::IfOlder; *attempt = 0; true }
            Some(MachineCommand::Reconnect) => { *attempt = 0; true }
        }
    }
}

/// Waits for the next bus event about machine `id`. Never resolves once the link is gone (the
/// connection loop handles that) or the bus closed.
async fn next_activity(bus: &mut broadcast::Receiver<BusEvent>, id: MachineId) {
    loop {
        match bus.recv().await {
            Ok(BusEvent::Connectivity {
                machine_id,
                connectivity,
            }) if machine_id == id && connectivity != Connectivity::Connected => {
                return std::future::pending().await;
            }
            Ok(event) if event.machine_id() == id => return,
            Ok(_) => {}
            // Missed events may have been ours.
            Err(broadcast::error::RecvError::Lagged(_)) => return,
            Err(broadcast::error::RecvError::Closed) => return std::future::pending().await,
        }
    }
}

/// Resolves once an automatic upgrade has started (the link is dropped): after the machine was
/// due and idle for the quiet period without any change on the bus, and not before
/// `retry_at`. Never resolves while auto-upgrade doesn't apply.
async fn auto_upgrade_due(
    hub: &Hub,
    id: MachineId,
    mut bus: broadcast::Receiver<BusEvent>,
    retry_at: Option<Instant>,
) {
    loop {
        if hub.upgrade_check(id) == UpgradeCheck::Idle {
            let quiet = Instant::now() + hub.services.tuning.auto_upgrade_quiet;
            let until = retry_at.map_or(quiet, |r| r.max(quiet));
            tokio::select! {
                () = tokio::time::sleep_until(until) => {
                    if hub.begin_auto_upgrade(id) {
                        return;
                    }
                }
                () = next_activity(&mut bus, id) => {}
            }
        } else {
            next_activity(&mut bus, id).await;
        }
    }
}

/// One SSH connection's life: bootstrap, daemon link, subscriptions, then wait.
async fn session(
    hub: &Arc<Hub>,
    id: MachineId,
    conn: &Arc<SshConnection>,
    key_id: KeyId,
    rx: &mut mpsc::Receiver<MachineCommand>,
    policy: &mut InstallPolicy,
    auto: &mut AutoUpgrade,
) -> End {
    loop {
        let probe = match bootstrap::probe_and_install(hub, id, conn).await {
            Ok(probe) => probe,
            Err(e) => return End::Failed(e),
        };
        let running = hub
            .lock()
            .machines
            .get(&id)
            .and_then(|e| e.record.version.clone());
        let mut installed_older = bootstrap::installed_is_older(hub, &probe);
        let upgrade = *policy == InstallPolicy::IfOlder
            && (installed_older || bundled_is_newer(hub, running.as_deref()));
        *policy = InstallPolicy::IfMissing;
        let link = if upgrade {
            match bootstrap::upgrade(hub, id, conn, running.as_deref()).await {
                Ok(link) => {
                    // The new daemon reported the bundled version.
                    installed_older = false;
                    Ok(link)
                }
                Err(e) if auto.attempting => {
                    // Keep the machine usable on the old daemon.
                    auto.failed(hub, id, &e);
                    continue;
                }
                Err(e) => return End::Failed(e),
            }
        } else {
            bootstrap::progress(hub, id, BootstrapStep::Start, "starting the daemon");
            daemon::open(hub, id, conn).await
        };
        let DaemonLink { client, hello } = match link {
            Ok(link) => link,
            Err(OpenError::Hello {
                error:
                    HelloError::Incompatible {
                        daemon_version,
                        protocol_version,
                    },
                ..
            }) => {
                auto.attempting = false;
                let update_available = bundled_is_newer(hub, Some(&daemon_version));
                hub.update_live(id, |l| {
                    l.connectivity = Connectivity::Incompatible;
                    l.bootstrap = None;
                    l.update_available = update_available;
                    l.last_error = Some(format!(
                        "controlroomd {daemon_version} speaks protocol {protocol_version}; upgrade it"
                    ));
                });
                tokio::select! {
                    reason = conn.closed() => return End::Lost(reason),
                    cmd = rx.recv() => match cmd {
                        Some(MachineCommand::Stop) | None => return End::Stop,
                        Some(MachineCommand::Reconnect) => return End::Reconnect,
                        Some(MachineCommand::Upgrade) => { *policy = InstallPolicy::IfOlder; continue }
                    }
                }
            }
            Err(OpenError::Hello {
                error: e @ HelloError::ControllerConflict { .. },
                ..
            }) => {
                return End::Action(
                    Connectivity::ActionRequired {
                        reason: ActionRequiredReason::ControllerConflict,
                        presented_host_key: None,
                    },
                    e.to_string(),
                );
            }
            Err(e) => {
                let reason = e.to_string();
                if auto.attempting {
                    auto.failed(hub, id, &reason);
                }
                return End::Failed(reason);
            }
        };

        let update_available =
            installed_older || bundled_is_newer(hub, Some(&hello.daemon_version));
        hub.connected(
            id,
            Link {
                daemon: client.clone(),
                ssh: conn.clone(),
                auth_key_id: key_id,
            },
            SeenUpdate {
                daemon_id: Some(hello.daemon_id),
                version: Some(hello.daemon_version.clone()),
                home: Some(probe.home.clone()),
                arch: Some(probe.arch.to_string()),
            },
            hello.capabilities.clone(),
            update_available,
        );
        connect::record_auth_key(hub, id, key_id);
        if !update_available {
            if auto.attempting {
                tracing::info!(machine = %id, version = %hello.daemon_version, "upgraded controlroomd automatically");
            }
            *auto = AutoUpgrade::default();
        } else if auto.attempting {
            let reason = format!(
                "controlroomd still reports {} after the upgrade",
                hello.daemon_version
            );
            auto.failed(hub, id, &reason);
        }
        if let Some(error) = auto.error.clone() {
            hub.update_live(id, |l| l.last_error = Some(error));
        }

        if let Err(e) = daemon::subscribe_machine(hub, id, &client).await {
            return End::Lost(e);
        }
        crate::rpc::resubscribe_sessions(hub, id, &client).await;
        let doctor = daemon::spawn_doctor(hub, id, &client);
        tokio::spawn(crate::rotation::migrate_pending(hub.clone(), id));

        let retry_at = auto.retry_at;
        let bus = hub.subscribe_bus();
        let end = tokio::select! {
            reason = client.closed() => Some(End::Lost(reason)),
            reason = conn.closed() => Some(End::Lost(reason)),
            cmd = rx.recv() => match cmd {
                Some(MachineCommand::Stop) | None => Some(End::Stop),
                Some(MachineCommand::Reconnect) => Some(End::Reconnect),
                Some(MachineCommand::Upgrade) => {
                    hub.disconnected(id, Connectivity::Connecting, None);
                    *policy = InstallPolicy::IfOlder;
                    None
                }
            },
            () = auto_upgrade_due(hub, id, bus, retry_at) => {
                tracing::info!(machine = %id, from = %hello.daemon_version, "upgrading controlroomd automatically");
                *policy = InstallPolicy::IfOlder;
                auto.attempting = true;
                None
            }
        };
        doctor.abort();
        client.close();
        if let Some(end) = end {
            return end;
        }
    }
}
