//! The ordered queue from the controller to the agent daemon's host bridge and policy:
//! answers (`hostops.respond`), forwarded host notifications (`hostops.notify`), machine reports
//! (`hostops.machines`) and profile reports (`profiles.report`), sent one at a time on the
//! current agent daemon connection.
//!
//! The queue is bounded. Answers wait for room; a notification that finds the queue full is
//! dropped (logged), like any notification of a connection that falls behind; a machine or
//! profile report that finds it full is sent once the queue drains (the latest report replaces
//! earlier ones). Without an agent daemon connection items are dropped: after the next `hello`
//! the agent daemon lists its pending requests again and gets fresh reports.
//!
//! An agent daemon that doesn't know `profiles.report` yet (`unsupported`, before profiles)
//! is logged and otherwise ignored; the report is sent again after the next `hello`.
//!
//! [`OutboundQueue::barrier`] waits until everything queued before it was answered, so a
//! command that depends on the machines or profiles (`session.create`) reaches an agent daemon
//! that already knows what the UI showed.

use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};

use controlroom_client_core::daemon::CallError;
use controlroom_protocol::ErrorData;
use controlroom_protocol::agentd::methods::{
    HostOpsMachines, HostOpsNotify, HostOpsRespond, ProfilesReport,
};
use controlroom_protocol::agentd::{
    HostOpsMachinesParams, HostOpsNotifyParams, HostOpsRespondParams, ProfilesReportParams,
};
use tokio::sync::{mpsc, oneshot};

use crate::hub::Hub;

/// Items buffered for the agent daemon.
const QUEUE: usize = 4096;

/// One item for the agent daemon.
#[derive(Debug)]
pub(crate) enum Outbound {
    /// A host's answer.
    Respond(HostOpsRespondParams),
    /// A host's notification.
    Notify(HostOpsNotifyParams),
    /// The machines.
    Machines(HostOpsMachinesParams),
    /// The profiles' policies.
    Profiles(ProfilesReportParams),
    /// Everything before it was sent: tell the waiter.
    Barrier(oneshot::Sender<()>),
}

/// The sending side, owned by the hub.
pub(crate) struct OutboundQueue {
    tx: mpsc::Sender<Outbound>,
    /// A machine report didn't fit: send the current one after the next item.
    machines_dirty: AtomicBool,
    /// A profile report didn't fit: send the current one after the next item.
    profiles_dirty: AtomicBool,
}

impl OutboundQueue {
    /// A queue and its receiver, for [`run`].
    pub(crate) fn new() -> (Self, mpsc::Receiver<Outbound>) {
        let (tx, rx) = mpsc::channel(QUEUE);
        (
            Self {
                tx,
                machines_dirty: AtomicBool::new(false),
                profiles_dirty: AtomicBool::new(false),
            },
            rx,
        )
    }

    /// Queues an answer, waiting for room.
    pub(crate) async fn respond(&self, answer: HostOpsRespondParams) {
        let _ = self.tx.send(Outbound::Respond(answer)).await;
    }

    /// Queues an answer without waiting (from under the hub lock).
    pub(crate) fn respond_now(&self, answer: HostOpsRespondParams) {
        if self.tx.try_send(Outbound::Respond(answer)).is_err() {
            tracing::warn!(
                "the agent daemon queue is full; an answer is dropped (it will be asked again)"
            );
        }
    }

    /// Queues a host notification; drops it when the queue is full.
    pub(crate) fn notify(&self, params: HostOpsNotifyParams) {
        if self.tx.try_send(Outbound::Notify(params)).is_err() {
            tracing::warn!("the agent daemon queue is full; dropping a host notification");
        }
    }

    /// Waits until every item queued so far (and a report that didn't fit) was sent and
    /// answered, or dropped for want of a connection.
    pub(crate) async fn barrier(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Outbound::Barrier(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }

    /// Queues a machine report.
    pub(crate) fn machines(&self, params: HostOpsMachinesParams) {
        if self.tx.try_send(Outbound::Machines(params)).is_err() {
            self.machines_dirty.store(true, Ordering::SeqCst);
        }
    }

    /// Queues a profile report.
    pub(crate) fn profiles(&self, params: ProfilesReportParams) {
        if self.tx.try_send(Outbound::Profiles(params)).is_err() {
            self.profiles_dirty.store(true, Ordering::SeqCst);
        }
    }
}

/// Sends queued items until the hub is gone.
pub(crate) async fn run(hub: Weak<Hub>, mut rx: mpsc::Receiver<Outbound>) {
    while let Some(item) = rx.recv().await {
        let Some(hub) = hub.upgrade() else { return };
        let barrier = match item {
            Outbound::Barrier(tx) => Some(tx),
            item => {
                send(&hub, item).await;
                None
            }
        };
        if hub.outbound.machines_dirty.swap(false, Ordering::SeqCst) {
            let machines = hub.lock().host_machines();
            send(&hub, Outbound::Machines(HostOpsMachinesParams { machines })).await;
        }
        if hub.outbound.profiles_dirty.swap(false, Ordering::SeqCst) {
            match hub.profile_report() {
                Ok(report) => send(&hub, Outbound::Profiles(report)).await,
                Err(e) => tracing::warn!(error = %e.message, "reading the profiles to report"),
            }
        }
        if let Some(tx) = barrier {
            let _ = tx.send(());
        }
    }
}

async fn send(hub: &Hub, item: Outbound) {
    let Some(client) = hub.agentd_client() else {
        return;
    };
    let result = match &item {
        Outbound::Respond(p) => client.call::<HostOpsRespond>(p).await.map(drop),
        Outbound::Notify(p) => client.call::<HostOpsNotify>(p).await.map(drop),
        Outbound::Machines(p) => client.call::<HostOpsMachines>(p).await.map(drop),
        Outbound::Profiles(p) => client.call::<ProfilesReport>(p).await.map(drop),
        Outbound::Barrier(_) => return,
    };
    match result {
        Ok(()) => {}
        Err(CallError::Rpc(e))
            if matches!(item, Outbound::Profiles(_))
                && matches!(e.data(), Some(ErrorData::Unsupported)) =>
        {
            tracing::info!(
                "the agent daemon doesn't take profile reports yet; it gets them after its next hello"
            );
        }
        Err(e) => tracing::debug!(error = %e, "sending to the agent daemon"),
    }
}
