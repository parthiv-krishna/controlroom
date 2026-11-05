//! Installing and upgrading the daemon on a connected machine, with live progress.
//!
//! - A fresh install uploads the bundled nono (unless the machine has its version) and the
//!   daemon, verifying each by running it before `current` points at the daemon (client-core's
//!   `install`). Each upload shows as live progress.
//! - An upgrade also stops the old daemon (supervisors and agents keep running), then checks the
//!   new one by opening the daemon link and saying `hello`; the new daemon must report the
//!   bundled version. Any failure rolls back to the previous version (client-core's
//!   `upgrade`). A newer daemon is never downgraded.

use std::sync::Arc;

use controlroom_client_core::bootstrap::{
    self, DaemonBinarySource, InstallDecision, InstallStep, ProbeResult, Program, UpgradeOutcome,
    Version,
};
use controlroom_client_core::ssh::SshConnection;
use controlroom_protocol::ids::MachineId;
use controlroom_protocol::ui::{BootstrapProgress, BootstrapStep};

use super::daemon::{self, DaemonLink};
use crate::hub::Hub;

pub(crate) fn progress(hub: &Hub, id: MachineId, step: BootstrapStep, message: impl Into<String>) {
    let message = message.into();
    hub.update_live(id, |l| {
        l.bootstrap = Some(BootstrapProgress { step, message })
    });
}

/// Reports each upload of an install (nono, then controlroomd) as an `install` step.
fn install_progress(hub: &Hub, id: MachineId) -> impl Fn(InstallStep<'_>) + Sync {
    move |step| progress(hub, id, BootstrapStep::Install, step.to_string())
}

/// Probes the machine; installs the bundled daemon when none is installed.
pub(crate) async fn probe_and_install(
    hub: &Hub,
    id: MachineId,
    conn: &SshConnection,
) -> Result<ProbeResult, String> {
    progress(hub, id, BootstrapStep::Probe, "probing the machine");
    let probe = bootstrap::probe(conn).await.map_err(|e| e.to_string())?;
    if probe.daemon_version_output.is_some() {
        return Ok(probe);
    }
    let source = hub.services.binaries.as_ref().map_err(|why| {
        format!("controlroomd is not installed on this machine and can't be installed: {why}")
    })?;
    let installed = bootstrap::install(conn, source.as_ref(), &probe, &install_progress(hub, id))
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!(
        machine = %id,
        version = %installed.version,
        verified_with = %installed.hashed_with,
        nono = %installed.nono.version,
        nono_uploaded = installed.nono.uploaded,
        "installed controlroomd"
    );
    bootstrap::probe(conn).await.map_err(|e| e.to_string())
}

/// Whether the bundled daemon is newer than `version`.
pub(crate) fn bundled_is_newer(hub: &Hub, version: Option<&str>) -> bool {
    let (Ok(source), Some(version)) = (hub.services.binaries.as_ref(), version) else {
        return false;
    };
    Version::parse(version).is_ok_and(|v| source.version(Program::Controlroomd) > &v)
}

/// Upgrades the machine to the bundled daemon (uploading nono too if its version changed) and returns the new daemon link. Rolls back on
/// any failure; the error then says so.
pub(crate) async fn upgrade(
    hub: &Arc<Hub>,
    id: MachineId,
    conn: &SshConnection,
    running: Option<&str>,
) -> Result<DaemonLink, String> {
    let source = hub
        .services
        .binaries
        .as_ref()
        .map_err(|why| format!("can't upgrade: {why}"))?;
    let bundled = source.version(Program::Controlroomd).to_string();
    progress(
        hub,
        id,
        BootstrapStep::Install,
        format!("upgrading controlroomd to {bundled}"),
    );
    let check = || async {
        progress(hub, id, BootstrapStep::Start, "starting the new daemon");
        let link = daemon::open(hub, id, conn)
            .await
            .map_err(|e| e.to_string())?;
        if link.hello.host_version != bundled {
            link.client.close();
            return Err(format!(
                "the new daemon reports {} instead of {bundled}",
                link.hello.host_version
            ));
        }
        Ok::<_, String>(link)
    };
    let stop_wait = hub.services.tuning.stop_wait;
    match bootstrap::upgrade(
        conn,
        source.as_ref(),
        &install_progress(hub, id),
        stop_wait,
        check,
    )
    .await
    {
        Ok(UpgradeOutcome::Upgraded {
            installed, checked, ..
        }) => {
            tracing::info!(machine = %id, version = %installed.version, "upgraded controlroomd");
            Ok(checked)
        }
        Ok(UpgradeOutcome::UpToDate) => {
            // Installed but not running: restart onto `current`.
            if bundled_is_newer(hub, running) {
                progress(hub, id, BootstrapStep::Start, "restarting the daemon");
                bootstrap::stop_daemon(conn, stop_wait)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            daemon::open(hub, id, conn).await.map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Whether `probe` shows an installed daemon older than the bundled one (never a newer one).
pub(crate) fn installed_is_older(hub: &Hub, probe: &ProbeResult) -> bool {
    hub.services.binaries.as_ref().is_ok_and(|source| {
        matches!(
            probe.decide(source.version(Program::Controlroomd)),
            InstallDecision::Upgrade { .. }
        )
    })
}
