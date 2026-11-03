//! One owner's live state: machines and their daemon links, the aggregated session cache, the
//! state feed, session subscription fan-out, connected UI clients and their focus, and the
//! event bus.
//!
//! The state sits behind one `std::sync::Mutex` that is never held across an `.await`. Store
//! writes that produce feed events run under it too (lock order: hub, then store), so the feed
//! order matches the store's. Subsystems react to changes through the [`bus`] instead of
//! adding state here.

mod bus;
mod clients;
mod feed;
mod machines;
mod sessions;
mod timeline;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use controlroom_client_core::daemon::DaemonClient;
use controlroom_client_core::ssh::SshConnection;
use controlroom_protocol::daemon::{
    DaemonNotification, DaemonSettings, DoctorReport, MachineSubscribeResult,
};
use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::{DeviceId, KeyId, MachineId, SessionId, SubscriptionId};
use controlroom_protocol::ui::notifications::{SessionEvents, StateEvents};
use controlroom_protocol::ui::{
    Connectivity, Machine, MachineSession, OnMachine, StateEvent, StateEventsParams, StateSnapshot,
};
use controlroom_protocol::{RpcError, RpcNotification};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};

pub(crate) use bus::BusEvent;
pub use clients::CloseReason;
pub(crate) use clients::{ClientEntry, ClientId, ClientSink};
pub(crate) use machines::{Link, MachineEntry, MachineLive, UpgradeCheck, host_key_info};
pub(crate) use timeline::{Delivery, Fanout, JoinPlan, TimelineKey};

use crate::config::Services;
use crate::errors::{internal, machine_unavailable, not_found, store_error};
use crate::keys;
use crate::machines::{self as machine_tasks, InstallPolicy, MachineCommand};
use crate::store::{MachineRow, OwnerId, OwnerRepo, SeenUpdate};

/// Commands buffered per machine task; a full queue drops the command (they are idempotent).
const COMMAND_QUEUE: usize = 16;

/// The hub's mutable state.
pub(crate) struct HubState {
    pub feed: feed::StateFeed,
    pub machines: BTreeMap<MachineId, MachineEntry>,
    pub clients: HashMap<ClientId, ClientEntry>,
    pub timelines: HashMap<TimelineKey, Fanout>,
    bus: broadcast::Sender<BusEvent>,
}

/// One owner's hub.
pub(crate) struct Hub {
    pub owner: OwnerId,
    pub services: Arc<Services>,
    state: Mutex<HubState>,
    /// Serializes key rotations.
    pub rotation: tokio::sync::Mutex<()>,
    /// Fires the owner's schedules.
    pub scheduler: crate::schedules::Scheduler,
    next_client: AtomicU64,
}

impl Hub {
    /// Loads the owner's machines, ensures the controlroom key, and starts every machine task
    /// and the notifier.
    pub(crate) fn start(services: Arc<Services>, owner: OwnerId) -> Result<Arc<Self>, RpcError> {
        services.store.ensure_owner(&owner).map_err(store_error)?;
        keys::ensure_controlroom_key(&services, &owner)?;
        let records = services
            .store
            .owner(&owner)
            .machines()
            .list()
            .map_err(store_error)?;
        let (bus, _) = broadcast::channel(bus::BUS_CAPACITY);
        let hub = Arc::new(Self {
            owner,
            services: services.clone(),
            state: Mutex::new(HubState {
                feed: feed::StateFeed::new(feed::RETAINED_CHANGES),
                machines: BTreeMap::new(),
                clients: HashMap::new(),
                timelines: HashMap::new(),
                bus,
            }),
            rotation: tokio::sync::Mutex::new(()),
            scheduler: crate::schedules::Scheduler::default(),
            next_client: AtomicU64::new(1),
        });
        if let Some(push) = services.push.clone() {
            let events = hub.subscribe_bus();
            tokio::spawn(crate::notify::run(Arc::downgrade(&hub), push, events));
        }
        for record in records {
            hub.spawn_machine(record, None, InstallPolicy::IfMissing);
        }
        hub.scheduler.start(&hub);
        Ok(hub)
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, HubState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The owner's store repositories.
    pub(crate) fn repo(&self) -> OwnerRepo<'_> {
        self.services.store.owner(&self.owner)
    }

    /// Subscribes to the event bus.
    pub(crate) fn subscribe_bus(&self) -> broadcast::Receiver<BusEvent> {
        self.lock().bus.subscribe()
    }

    // --- Machines ------------------------------------------------------------------------

    /// Registers a machine and starts its supervising task.
    pub(crate) fn spawn_machine(
        self: &Arc<Self>,
        record: MachineRow,
        initial: Option<(SshConnection, KeyId)>,
        policy: InstallPolicy,
    ) {
        let id = record.id;
        let (control, rx) = mpsc::channel(COMMAND_QUEUE);
        {
            let mut st = self.lock();
            st.machines.insert(
                id,
                MachineEntry {
                    record,
                    live: MachineLive::default(),
                    link: None,
                    machine_sub: None,
                    rev: None,
                    sessions: BTreeMap::new(),
                    control,
                    task: None,
                    sub_lock: Arc::new(tokio::sync::Mutex::new(())),
                },
            );
            st.emit_machine(id);
        }
        let task = tokio::spawn(machine_tasks::run(self.clone(), id, rx, initial, policy));
        if let Some(entry) = self.lock().machines.get_mut(&id) {
            entry.task = Some(task);
        }
    }

    /// Stops a machine's task and forgets it; its sessions leave the feed.
    pub(crate) async fn remove_machine(&self, id: MachineId) {
        let task = {
            let mut st = self.lock();
            let Some(mut entry) = st.machines.remove(&id) else {
                return;
            };
            let _ = entry.control.try_send(MachineCommand::Stop);
            if let Some(link) = &entry.link {
                link.daemon.close();
            }
            st.timelines.retain(|key, _| key.machine_id != id);
            st.emit(StateEvent::MachineRemoved { machine_id: id });
            entry.task.take()
        };
        // Its schedules are disabled on the next pass.
        self.scheduler.wake();
        if let Some(task) = task {
            stop_task(task).await;
        }
    }

    /// Stops every machine task.
    pub(crate) async fn shutdown(&self) {
        self.scheduler.stop().await;
        let tasks: Vec<_> = {
            let mut st = self.lock();
            for client in st.clients.values() {
                client.sink.close(CloseReason::Shutdown);
            }
            st.machines
                .values_mut()
                .filter_map(|entry| {
                    let _ = entry.control.try_send(MachineCommand::Stop);
                    entry.task.take()
                })
                .collect()
        };
        for task in tasks {
            stop_task(task).await;
        }
    }

    /// Sends a command to a machine's task.
    pub(crate) fn command(&self, id: MachineId, command: MachineCommand) -> Result<(), RpcError> {
        let st = self.lock();
        let entry = st
            .machines
            .get(&id)
            .ok_or_else(|| not_found(Resource::Machine, id))?;
        if entry.control.try_send(command).is_err() {
            tracing::debug!(machine = %id, ?command, "machine command queue full; dropped");
        }
        Ok(())
    }

    /// The connected daemon of a machine, or `machine_unavailable`.
    pub(crate) fn daemon(&self, id: MachineId) -> Result<DaemonClient, RpcError> {
        let st = self.lock();
        let entry = st
            .machines
            .get(&id)
            .ok_or_else(|| not_found(Resource::Machine, id))?;
        match &entry.link {
            Some(link) if entry.live.connectivity == Connectivity::Connected => {
                Ok(link.daemon.clone())
            }
            _ => Err(machine_unavailable(id, entry.live.connectivity.clone())),
        }
    }

    /// A machine's connectivity (`unreachable` once it is gone).
    pub(crate) fn connectivity(&self, id: MachineId) -> Connectivity {
        self.lock()
            .machines
            .get(&id)
            .map_or(Connectivity::Unreachable, |e| e.live.connectivity.clone())
    }

    /// A machine's protocol view.
    pub(crate) fn machine_view(&self, id: MachineId) -> Result<Machine, RpcError> {
        self.lock()
            .machines
            .get(&id)
            .map(MachineEntry::view)
            .ok_or_else(|| not_found(Resource::Machine, id))
    }

    /// A machine's display name.
    pub(crate) fn machine_name(&self, id: MachineId) -> Option<String> {
        self.lock().machines.get(&id).map(|e| e.record.name.clone())
    }

    /// A cached session summary.
    pub(crate) fn session(
        &self,
        machine: MachineId,
        session: SessionId,
    ) -> Option<controlroom_protocol::session::SessionSummary> {
        self.lock()
            .machines
            .get(&machine)?
            .sessions
            .get(&session)
            .cloned()
    }

    /// Updates a machine's live status; emits a change if anything differs.
    pub(crate) fn update_live(&self, id: MachineId, f: impl FnOnce(&mut MachineLive)) {
        let mut st = self.lock();
        let Some(entry) = st.machines.get_mut(&id) else {
            return;
        };
        let before = entry.live.clone();
        f(&mut entry.live);
        if entry.live == before {
            return;
        }
        let connectivity = (entry.live.connectivity != before.connectivity)
            .then(|| entry.live.connectivity.clone());
        st.emit_machine(id);
        if let Some(connectivity) = connectivity {
            let _ = st.bus.send(BusEvent::Connectivity {
                machine_id: id,
                connectivity,
            });
        }
    }

    /// Re-reads a machine's record from the store and emits it. Callers write the columns they
    /// changed first; the record is never written back whole.
    pub(crate) fn reload_record(&self, id: MachineId) -> Result<(), RpcError> {
        let mut st = self.lock();
        let record = self.repo().machines().get(id).map_err(store_error)?;
        if let (Some(record), Some(entry)) = (record, st.machines.get_mut(&id)) {
            entry.record = record;
            st.emit_machine(id);
            let _ = st.bus.send(BusEvent::MachineChanged { machine_id: id });
        }
        Ok(())
    }

    /// Stores and shows a machine's doctor report.
    pub(crate) fn set_doctor(&self, id: MachineId, report: &DoctorReport) {
        if let Err(e) = self.repo().machines().set_doctor(id, report) {
            tracing::warn!(error = %e, "storing the doctor report");
        }
        let _ = self.reload_record(id);
    }

    /// Stores and shows a machine's daemon settings.
    pub(crate) fn set_daemon_settings(&self, id: MachineId, settings: &DaemonSettings) {
        let changed = self
            .lock()
            .machines
            .get(&id)
            .is_some_and(|e| e.record.daemon_settings.as_ref() != Some(settings));
        if !changed {
            return;
        }
        if let Err(e) = self.repo().machines().set_daemon_settings(id, settings) {
            tracing::warn!(error = %e, "storing daemon settings");
        }
        let _ = self.reload_record(id);
    }

    pub(crate) fn upgrade_check(&self, id: MachineId) -> UpgradeCheck {
        self.lock()
            .machines
            .get(&id)
            .map_or(UpgradeCheck::Off, MachineEntry::upgrade_check)
    }

    /// Starts an automatic upgrade if it is still due and the machine idle. Under the lock, so
    /// no UI call reaches the daemon afterwards: the link is dropped as on a disconnect.
    pub(crate) fn begin_auto_upgrade(&self, id: MachineId) -> bool {
        let mut st = self.lock();
        let due = st
            .machines
            .get(&id)
            .is_some_and(|e| e.upgrade_check() == UpgradeCheck::Idle);
        if due {
            st.disconnected(id, Connectivity::Connecting, None);
        }
        due
    }

    /// The SSH connection of a connected machine and the key it authenticated with.
    pub(crate) fn link_ssh(&self, id: MachineId) -> Option<(Arc<SshConnection>, KeyId)> {
        let st = self.lock();
        let link = st.machines.get(&id)?.link.as_ref()?;
        Some((link.ssh.clone(), link.auth_key_id))
    }

    /// The machine-feed revision to resume from.
    pub(crate) fn since_rev(&self, id: MachineId) -> Option<controlroom_protocol::Rev> {
        self.lock().machines.get(&id).and_then(|e| e.rev)
    }

    // --- Connection lifecycle (the machine task) --------------------------------------------

    /// The daemon said hello: records the link and what we learned. A different daemon
    /// installation drops the cached sessions.
    pub(crate) fn connected(
        &self,
        id: MachineId,
        link: Link,
        seen: SeenUpdate,
        update_available: bool,
    ) {
        let now = controlroom_protocol::UnixMillis::now();
        if let Err(e) = self.repo().machines().record_seen(id, &seen, now) {
            tracing::warn!(error = %e, "recording machine contact");
        }
        let record = self.repo().machines().get(id).ok().flatten();
        let mut st = self.lock();
        let Some(entry) = st.machines.get_mut(&id) else {
            return;
        };
        let replaced = entry
            .record
            .daemon_id
            .is_some_and(|d| Some(d) != seen.daemon_id);
        if let Some(record) = record {
            entry.record = record;
        }
        let gone: Vec<SessionId> = if replaced {
            entry.rev = None;
            std::mem::take(&mut entry.sessions).into_keys().collect()
        } else {
            Vec::new()
        };
        let was_stale = entry.stale();
        entry.link = Some(link);
        entry.machine_sub = None;
        entry.live = MachineLive {
            connectivity: Connectivity::Connected,
            update_available,
            last_error: None,
            bootstrap: None,
        };
        for session_id in gone {
            st.session_removed(id, session_id);
        }
        st.emit_machine(id);
        if was_stale {
            st.emit_sessions(id);
        }
        let _ = st.bus.send(BusEvent::Connectivity {
            machine_id: id,
            connectivity: Connectivity::Connected,
        });
    }

    /// The connection is gone; sessions stay cached, marked stale.
    pub(crate) fn disconnected(
        &self,
        id: MachineId,
        connectivity: Connectivity,
        error: Option<String>,
    ) {
        self.lock().disconnected(id, connectivity, error);
    }

    /// The `machine.subscribe` response hook: runs before any `machine.events` of it.
    pub(crate) fn machine_subscribed(&self, id: MachineId, result: MachineSubscribeResult) {
        let settings = {
            let mut st = self.lock();
            sessions::subscribed(&mut st, id, result)
        };
        if let Some(settings) = settings {
            self.set_daemon_settings(id, &settings);
        }
    }

    /// Routes a daemon notification (called on the daemon reader task).
    pub(crate) fn on_daemon_notification(&self, id: MachineId, n: RpcNotification<Value>) {
        match DaemonNotification::from_parts(&n.method, n.params) {
            Ok(DaemonNotification::MachineEvents(p)) => {
                let settings = {
                    let mut st = self.lock();
                    sessions::events(&mut st, id, p)
                };
                if let Some(settings) = settings {
                    self.set_daemon_settings(id, &settings);
                }
            }
            Ok(DaemonNotification::SessionEvents(p)) => {
                let st = &mut *self.lock();
                let key = st
                    .timelines
                    .iter()
                    .find(|(k, f)| k.machine_id == id && f.daemon_sub() == Some(p.subscription_id))
                    .map(|(k, _)| *k);
                if let Some(key) = key {
                    let deliveries = st
                        .timelines
                        .get_mut(&key)
                        .map(|f| f.deliver(&p))
                        .unwrap_or_default();
                    st.send_timeline(key, deliveries);
                }
            }
            Ok(DaemonNotification::OutboxItems(_)) => {
                // The mailbox broker arrives in P7; nothing subscribes to the outbox yet.
            }
            Err(e) => {
                tracing::warn!(method = %n.method, error = %e.message, "bad daemon notification")
            }
        }
    }

    // --- UI clients ---------------------------------------------------------------------

    /// Registers a UI connection.
    pub(crate) fn add_client(&self, sink: ClientSink, auth_token_hash: Option<String>) -> ClientId {
        let id = ClientId(self.next_client.fetch_add(1, Ordering::Relaxed));
        self.lock().clients.insert(
            id,
            ClientEntry {
                sink,
                state_subs: HashMap::new(),
                focus: None,
                device: None,
                auth_token_hash,
            },
        );
        id
    }

    /// Drops a client's subscriptions and focus; ends daemon session subscriptions nobody
    /// needs anymore.
    pub(crate) fn remove_client(&self, id: ClientId) {
        let ended = {
            let mut st = self.lock();
            st.clients.remove(&id);
            let mut ended = Vec::new();
            st.timelines.retain(|key, fanout| {
                if fanout.remove_client(id) {
                    ended.push((key.machine_id, fanout.daemon_sub()));
                    false
                } else {
                    true
                }
            });
            ended
        };
        for (machine_id, sub) in ended {
            if let Some(sub) = sub {
                self.unsubscribe_daemon(machine_id, sub);
            }
        }
    }

    /// Ends a daemon subscription in the background.
    pub(crate) fn unsubscribe_daemon(&self, machine_id: MachineId, sub: SubscriptionId) {
        if let Ok(daemon) = self.daemon(machine_id) {
            tokio::spawn(async move {
                let params = controlroom_protocol::daemon::UnsubscribeParams {
                    subscription_id: sub,
                };
                let _ = daemon
                    .call::<controlroom_protocol::daemon::methods::Unsubscribe>(&params)
                    .await;
            });
        }
    }

    /// Closes every client matching `pred`.
    pub(crate) fn close_clients(&self, reason: CloseReason, pred: impl Fn(&ClientEntry) -> bool) {
        for client in self.lock().clients.values().filter(|c| pred(c)) {
            client.sink.close(reason);
        }
    }

    /// Devices with a visible client focused on a session.
    pub(crate) fn focused_devices(&self, machine: MachineId, session: SessionId) -> Vec<DeviceId> {
        self.lock()
            .clients
            .values()
            .filter_map(|c| {
                let f = c.focus.as_ref()?;
                let on = f
                    .session
                    .as_ref()
                    .is_some_and(|s| s.machine_id == machine && s.session_id == session);
                (f.visible && on).then(|| f.device_id.clone())
            })
            .collect()
    }

    /// Emits a device's current state (or its removal).
    pub(crate) fn device_changed(&self, device: &DeviceId) {
        let mut st = self.lock();
        match self.repo().devices().get(device) {
            Ok(Some(device)) => st.emit(StateEvent::DeviceUpserted { device }),
            Ok(None) => st.emit(StateEvent::DeviceRemoved {
                device_id: device.clone(),
            }),
            Err(e) => tracing::warn!(error = %e, "reading a device"),
        }
    }

    /// The full state, for a snapshot.
    pub(crate) fn snapshot(&self, st: &HubState) -> Result<StateSnapshot, RpcError> {
        let repo = self.repo();
        Ok(StateSnapshot {
            machines: st.machines.values().map(MachineEntry::view).collect(),
            sessions: st
                .machines
                .iter()
                .flat_map(|(id, entry)| {
                    entry.sessions.values().map(|s| MachineSession {
                        machine_id: *id,
                        stale: entry.stale(),
                        session: s.clone(),
                    })
                })
                .collect(),
            projects: repo.projects().list().map_err(store_error)?,
            tasks: repo.tasks().list().map_err(store_error)?,
            grants: repo.grants().list().map_err(store_error)?,
            schedules: repo.schedules().list().map_err(store_error)?,
            devices: repo.devices().list().map_err(store_error)?,
        })
    }

    /// A store-backed task view, or `not_found`.
    pub(crate) fn task(
        &self,
        id: controlroom_protocol::ids::TaskId,
    ) -> Result<controlroom_protocol::ui::Task, RpcError> {
        self.repo()
            .tasks()
            .get(id)
            .map_err(store_error)?
            .ok_or_else(|| not_found(Resource::Task, id))
    }
}

impl HubState {
    /// Records a change and fans it out to every state subscription.
    pub(crate) fn emit(&mut self, event: StateEvent) {
        let change = self.feed.push(event);
        for client in self.clients.values_mut() {
            for (sub, pending) in client.state_subs.iter_mut() {
                match pending {
                    Some(buffer) => buffer.push(change.clone()),
                    None => client.sink.notify::<StateEvents>(StateEventsParams {
                        subscription_id: *sub,
                        changes: vec![change.clone()],
                    }),
                }
            }
        }
    }

    /// Emits a machine's current view.
    pub(crate) fn emit_machine(&mut self, id: MachineId) {
        if let Some(entry) = self.machines.get(&id) {
            let machine = Box::new(entry.view());
            self.emit(StateEvent::MachineUpserted { machine });
        }
    }

    /// Emits every session of a machine (after its staleness changed).
    fn emit_sessions(&mut self, id: MachineId) {
        let Some(entry) = self.machines.get(&id) else {
            return;
        };
        let stale = entry.stale();
        let sessions: Vec<_> = entry
            .sessions
            .values()
            .map(|s| MachineSession {
                machine_id: id,
                stale,
                session: s.clone(),
            })
            .collect();
        for session in sessions {
            self.emit(StateEvent::SessionUpserted {
                session: Box::new(session),
            });
        }
    }

    fn session_removed(&mut self, machine_id: MachineId, session_id: SessionId) {
        let _ = self.bus.send(BusEvent::SessionRemoved {
            machine_id,
            session_id,
        });
        self.emit(StateEvent::SessionRemoved {
            machine_id,
            session_id,
        });
    }

    /// See [`Hub::disconnected`].
    fn disconnected(&mut self, id: MachineId, connectivity: Connectivity, error: Option<String>) {
        let Some(entry) = self.machines.get_mut(&id) else {
            return;
        };
        let was_connected = entry.live.connectivity == Connectivity::Connected;
        if let Some(link) = entry.link.take() {
            link.daemon.close();
        }
        entry.machine_sub = None;
        entry.live.connectivity = connectivity.clone();
        entry.live.bootstrap = None;
        if error.is_some() {
            entry.live.last_error = error;
        }
        for (key, fanout) in self.timelines.iter_mut() {
            if key.machine_id == id {
                fanout.disconnected();
            }
        }
        self.emit_machine(id);
        if was_connected {
            self.emit_sessions(id);
        }
        let _ = self.bus.send(BusEvent::Connectivity {
            machine_id: id,
            connectivity,
        });
    }

    /// Sends timeline deliveries to their clients.
    pub(crate) fn send_timeline(&self, key: TimelineKey, deliveries: Vec<Delivery>) {
        for d in deliveries {
            let Some(client) = self.clients.get(&d.client) else {
                continue;
            };
            client.sink.notify::<SessionEvents>(OnMachine::new(
                key.machine_id,
                controlroom_protocol::daemon::SessionEventsParams {
                    subscription_id: d.subscription_id,
                    id: key.session_id,
                    head_seq: d.batch.head_seq,
                    ops: d.batch.ops,
                    raw: d.batch.raw,
                    detail: d.batch.detail,
                },
            ));
        }
    }
}

/// Waits for a stopped task, aborting it if it doesn't end in time.
async fn stop_task(mut task: tokio::task::JoinHandle<()>) {
    if tokio::time::timeout(Duration::from_secs(5), &mut task)
        .await
        .is_err()
    {
        task.abort();
        let _ = task.await;
    }
}

/// The error for a request whose connection closed while it ran.
pub(crate) fn client_gone() -> RpcError {
    internal("the connection closed")
}
