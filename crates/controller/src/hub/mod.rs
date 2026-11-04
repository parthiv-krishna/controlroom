//! One owner's live state: machines and their host daemon links, the agent daemon's global
//! sessions, the UI state feed, session subscription fan-out, the host router's pending
//! requests, connected UI clients and their focus, and the event bus.
//!
//! The state sits behind one `std::sync::Mutex` that is never held across an `.await`. Store
//! writes that produce feed events run under it too (lock order: hub, then store), so the feed
//! order matches the store's. Subsystems react to changes through the [`bus`] instead of
//! adding state here.

mod bus;
mod clients;
mod feed;
mod global;
mod machines;
mod timeline;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use controlroom_client_core::daemon::DaemonClient;
use controlroom_client_core::ssh::SshConnection;
use controlroom_protocol::agentd::SessionEventsParams;
use controlroom_protocol::agentd::{HostMachine, HostOpsMachinesParams, HostOpsNotifyParams};
use controlroom_protocol::error::Resource;
use controlroom_protocol::host::{DoctorReport, HostNotification};
use controlroom_protocol::ids::{
    DeviceId, GrantId, KeyId, MachineId, ProjectId, ScheduleId, SessionId, SubscriptionId, TaskId,
};
use controlroom_protocol::session::SessionSummary;
use controlroom_protocol::ui::notifications::{SessionEvents, StateEvents};
use controlroom_protocol::ui::{
    Connectivity, Device, Machine, Project, Schedule, StateChange, StateEvent, StateEventsParams,
    StateSnapshot, Task,
};
use controlroom_protocol::{ErrorData, RpcError, RpcNotification};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};

pub(crate) use bus::BusEvent;
pub use clients::CloseReason;
pub(crate) use clients::{ClientEntry, ClientId, ClientSink, Focus};
pub(crate) use global::GlobalSessions;
pub(crate) use machines::{Link, MachineEntry, MachineLive, UpgradeCheck, host_key_info};
pub(crate) use timeline::{Delivery, Fanout, JoinPlan, TimelineKey};

use crate::config::Services;
use crate::errors::{internal, machine_unavailable, not_found, store_error};
use crate::keys;
use crate::machines::{self as machine_tasks, InstallPolicy, MachineCommand};
use crate::router::Router;
use crate::router::outbound::{self, OutboundQueue};
use crate::store::{MachineRow, OwnerId, OwnerRepo, SeenUpdate};

/// Commands buffered per machine task; a full queue drops the command (they are idempotent).
const COMMAND_QUEUE: usize = 16;

/// The hub's mutable state.
pub(crate) struct HubState {
    /// The UI state feed.
    pub feed: feed::StateFeed,
    pub machines: BTreeMap<MachineId, MachineEntry>,
    /// The agent daemon's sessions.
    pub global: GlobalSessions,
    /// The connection to the agent daemon, while it is up.
    pub agentd: Option<DaemonClient>,
    /// The host router's pending requests.
    pub router: Router,
    /// The last machine report sent to the agent daemon.
    reported_machines: Vec<HostMachine>,
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
    /// Serializes session subscription changes on the agent daemon.
    pub agentd_sub_lock: Arc<tokio::sync::Mutex<()>>,
    /// Fires the owner's schedules.
    pub scheduler: crate::schedules::Scheduler,
    /// The host router's ordered queue to the agent daemon.
    pub outbound: OutboundQueue,
    /// The agent daemon link and outbound queue tasks.
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    next_client: AtomicU64,
}

impl Hub {
    /// Loads the owner's machines, ensures the controlroom key, and starts every machine task,
    /// the notifier, the scheduler and the agent daemon link.
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
        let (outbound, outbound_rx) = OutboundQueue::new();
        let hub = Arc::new(Self {
            owner,
            services: services.clone(),
            state: Mutex::new(HubState {
                feed: feed::StateFeed::new(feed::RETAINED_CHANGES),
                machines: BTreeMap::new(),
                global: GlobalSessions::default(),
                agentd: None,
                router: Router::default(),
                reported_machines: Vec::new(),
                clients: HashMap::new(),
                timelines: HashMap::new(),
                bus,
            }),
            rotation: tokio::sync::Mutex::new(()),
            agentd_sub_lock: Arc::default(),
            scheduler: crate::schedules::Scheduler::default(),
            outbound,
            tasks: Mutex::new(Vec::new()),
            next_client: AtomicU64::new(1),
        });
        {
            let mut st = hub.lock();
            hub.refresh_membership(&mut st);
        }
        if let Some(push) = services.push.clone() {
            let events = hub.subscribe_bus();
            tokio::spawn(crate::notify::run(Arc::downgrade(&hub), push, events));
        }
        for record in records {
            hub.spawn_machine(record, None, InstallPolicy::IfMissing);
        }
        hub.scheduler.start(&hub);
        let mut tasks = vec![tokio::spawn(outbound::run(
            Arc::downgrade(&hub),
            outbound_rx,
        ))];
        if let Some(socket) = services.agentd_socket.clone() {
            tasks.push(tokio::spawn(crate::agentd::run(
                Arc::downgrade(&hub),
                socket,
            )));
        }
        *hub.tasks.lock().unwrap_or_else(|p| p.into_inner()) = tasks;
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
                    control,
                    task: None,
                    epoch: None,
                },
            );
            st.emit_machine(id);
            self.report_machines_locked(&mut st, false);
        }
        let task = tokio::spawn(machine_tasks::run(self.clone(), id, rx, initial, policy));
        if let Some(entry) = self.lock().machines.get_mut(&id) {
            entry.task = Some(task);
        }
    }

    /// Stops a machine's task and forgets it; its pending host requests are answered
    /// `not_found`.
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
            st.emit(StateEvent::MachineRemoved { machine_id: id });
            self.host_removed(&mut st, id);
            self.report_machines_locked(&mut st, false);
            entry.task.take()
        };
        if let Some(task) = task {
            stop_task(task).await;
        }
    }

    /// Stops every machine task and the agent daemon link.
    pub(crate) async fn shutdown(&self) {
        self.scheduler.stop().await;
        for task in self
            .tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
        {
            task.abort();
        }
        let tasks: Vec<_> = {
            let mut st = self.lock();
            for client in st.clients.values() {
                client.sink.close(CloseReason::Shutdown);
            }
            if let Some(agentd) = st.agentd.take() {
                agentd.close();
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

    /// The connected host daemon of a machine (`fs.list`, `doctor.report`), or
    /// `machine_unavailable`.
    pub(crate) fn host(&self, id: MachineId) -> Result<DaemonClient, RpcError> {
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
    pub(crate) fn session(&self, session: SessionId) -> Option<SessionSummary> {
        self.lock().global.sessions.get(&session).cloned()
    }

    /// Updates a machine's live status; emits a change if anything differs.
    pub(crate) fn update_live(&self, id: MachineId, f: impl FnOnce(&mut MachineLive)) {
        let mut st = self.lock();
        let Some(entry) = st.machines.get_mut(&id) else {
            return;
        };
        let before = entry.live.clone();
        f(&mut entry.live);
        entry.live.track_since(&before.connectivity);
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
            self.report_machines_locked(&mut st, false);
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
            self.report_machines_locked(&mut st, false);
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

    pub(crate) fn upgrade_check(&self, id: MachineId) -> UpgradeCheck {
        self.lock().upgrade_check(id)
    }

    /// Starts an automatic upgrade if it is still due and the machine idle. Under the lock, so
    /// no UI call reaches the daemon afterwards: the link is dropped as on a disconnect.
    pub(crate) fn begin_auto_upgrade(&self, id: MachineId) -> bool {
        let mut st = self.lock();
        let due = st.upgrade_check(id) == UpgradeCheck::Idle;
        if due {
            self.disconnected_locked(&mut st, id, Connectivity::Connecting, None);
        }
        due
    }

    /// The SSH connection of a connected machine and the key it authenticated with.
    pub(crate) fn link_ssh(&self, id: MachineId) -> Option<(Arc<SshConnection>, KeyId)> {
        let st = self.lock();
        let link = st.machines.get(&id)?.link.as_ref()?;
        Some((link.ssh.clone(), link.auth_key_id))
    }

    // --- Connection lifecycle (the machine task) --------------------------------------------

    /// The host daemon said hello: records the link and what we learned, and sends it its
    /// waiting requests.
    pub(crate) fn connected(
        self: &Arc<Self>,
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
        if let Some(record) = record {
            entry.record = record;
        }
        entry.epoch = Some(link.epoch);
        entry.link = Some(link);
        // Every hello is a new connection, even after a quick drop.
        entry.live = MachineLive {
            connectivity: Connectivity::Connected,
            update_available,
            last_error: None,
            bootstrap: None,
            since: now,
        };
        st.emit_machine(id);
        let _ = st.bus.send(BusEvent::Connectivity {
            machine_id: id,
            connectivity: Connectivity::Connected,
        });
        // The agent daemon learns of the new connection before its notifications arrive.
        self.report_machines_locked(&mut st, false);
        self.host_connected(&mut st, id);
    }

    /// The connection is gone; host requests wait for the next connection.
    pub(crate) fn disconnected(
        &self,
        id: MachineId,
        connectivity: Connectivity,
        error: Option<String>,
    ) {
        let mut st = self.lock();
        self.disconnected_locked(&mut st, id, connectivity, error);
    }

    fn disconnected_locked(
        &self,
        st: &mut HubState,
        id: MachineId,
        connectivity: Connectivity,
        error: Option<String>,
    ) {
        st.disconnected(id, connectivity, error);
        self.host_disconnected(st, id);
        self.report_machines_locked(st, false);
    }

    /// Forwards a host daemon's notification to the agent daemon (called on the host reader
    /// task, so the order is the host's).
    pub(crate) fn on_host_notification(&self, id: MachineId, n: RpcNotification<Value>) {
        match HostNotification::from_parts(&n.method, n.params) {
            Ok(notification) => self.outbound.notify(HostOpsNotifyParams {
                machine_id: id,
                notification,
            }),
            Err(e) => {
                tracing::warn!(machine = %id, method = %n.method, error = %e.message, "bad host notification")
            }
        }
    }

    /// Routes a session subscription's events to its fanout.
    pub(crate) fn timeline_events(&self, p: &SessionEventsParams) {
        let st = &mut *self.lock();
        let key = st
            .timelines
            .iter()
            .find(|(_, f)| f.daemon_sub() == Some(p.subscription_id))
            .map(|(k, _)| *k);
        if let Some(key) = key {
            let deliveries = st
                .timelines
                .get_mut(&key)
                .map(|f| f.deliver(p))
                .unwrap_or_default();
            st.send_timeline(key, deliveries);
        }
    }

    // --- The agent daemon -------------------------------------------------------------------

    /// The agent daemon said hello.
    pub(crate) fn agentd_connected(&self, client: DaemonClient) {
        let mut st = self.lock();
        st.agentd = Some(client);
        let _ = st.bus.send(BusEvent::Agentd { connected: true });
    }

    /// The agent daemon connection `client` is gone (a newer one is left alone).
    pub(crate) fn agentd_disconnected(&self, client: &DaemonClient) {
        let mut st = self.lock();
        let current = st
            .agentd
            .as_ref()
            .is_some_and(|c| c.same_connection(client));
        if !current {
            return;
        }
        st.agentd = None;
        st.global.sub = None;
        for fanout in st.timelines.values_mut() {
            fanout.disconnected();
        }
        self.hostops_reset(&mut st);
        let _ = st.bus.send(BusEvent::Agentd { connected: false });
    }

    /// The agent daemon connection, or `timeout` (the protocol's answer while it can't be
    /// reached).
    pub(crate) fn agentd(&self) -> Result<DaemonClient, RpcError> {
        self.agentd_client().ok_or_else(agentd_unavailable)
    }

    /// The agent daemon connection, while it is up.
    pub(crate) fn agentd_client(&self) -> Option<DaemonClient> {
        self.lock().agentd.clone()
    }

    /// Reports the machines to the agent daemon if they changed since the last report (or
    /// always, after a new `hello`).
    pub(crate) fn report_machines(&self, force: bool) {
        let mut st = self.lock();
        self.report_machines_locked(&mut st, force);
    }

    fn report_machines_locked(&self, st: &mut HubState, force: bool) {
        let machines = st.host_machines();
        if force || machines != st.reported_machines {
            st.reported_machines = machines.clone();
            self.outbound.machines(HostOpsMachinesParams { machines });
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

    /// Drops a client's subscriptions and focus; ends session subscriptions nobody needs
    /// anymore.
    pub(crate) fn remove_client(&self, id: ClientId) {
        let ended = {
            let mut st = self.lock();
            st.clients.remove(&id);
            let mut ended = Vec::new();
            st.timelines.retain(|_, fanout| {
                if fanout.remove_client(id) {
                    ended.extend(fanout.daemon_sub());
                    false
                } else {
                    true
                }
            });
            ended
        };
        for sub in ended {
            self.unsubscribe(sub);
        }
    }

    /// Ends a session subscription on the agent daemon in the background.
    pub(crate) fn unsubscribe(&self, sub: SubscriptionId) {
        if let Ok(daemon) = self.agentd() {
            tokio::spawn(async move {
                let params = controlroom_protocol::agentd::UnsubscribeParams {
                    subscription_id: sub,
                };
                let _ = daemon
                    .call::<controlroom_protocol::agentd::methods::Unsubscribe>(&params)
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
    pub(crate) fn focused_devices(&self, session: SessionId) -> Vec<DeviceId> {
        self.lock()
            .clients
            .values()
            .filter_map(|c| {
                let f = c.focus.as_ref()?;
                (f.visible && f.session_id == Some(session)).then(|| f.device_id.clone())
            })
            .collect()
    }

    /// Emits a device's current state (or its removal).
    pub(crate) fn device_changed(&self, device: &DeviceId) {
        let mut st = self.lock();
        match self.repo().devices().get(device) {
            Ok(Some(device)) => st.emit_device(device),
            Ok(None) => st.device_removed(device),
            Err(e) => tracing::warn!(error = %e, "reading a device"),
        }
    }

    /// The full state, for a snapshot.
    pub(crate) fn snapshot(&self, st: &HubState) -> Result<StateSnapshot, RpcError> {
        let repo = self.repo();
        Ok(StateSnapshot {
            machines: st.machines.values().map(MachineEntry::view).collect(),
            sessions: st.global.feed(),
            overlaps: st.global.overlaps.clone(),
            projects: repo.projects().list().map_err(store_error)?,
            tasks: repo.tasks().list().map_err(store_error)?,
            grants: repo.grants().list().map_err(store_error)?,
            schedules: repo.schedules().list().map_err(store_error)?,
            devices: repo.devices().list().map_err(store_error)?,
        })
    }

    /// A stored task, or `not_found`.
    pub(crate) fn task(&self, id: TaskId) -> Result<Task, RpcError> {
        self.repo()
            .tasks()
            .get(id)
            .map_err(store_error)?
            .ok_or_else(|| not_found(Resource::Task, id))
    }

    // --- Controller data ------------------------------------------------------------------

    /// Emits a task; its sessions' project membership follows.
    pub(crate) fn emit_task(&self, st: &mut HubState, task: &Task) {
        st.emit(StateEvent::TaskUpserted { task: task.clone() });
        self.refresh_membership(st);
    }

    /// Emits a task's removal.
    pub(crate) fn emit_task_removed(&self, st: &mut HubState, task_id: TaskId) {
        st.emit(StateEvent::TaskRemoved { task_id });
        self.refresh_membership(st);
    }

    /// Emits a project.
    pub(crate) fn emit_project(&self, st: &mut HubState, project: &Project) {
        st.emit(StateEvent::ProjectUpserted {
            project: project.clone(),
        });
    }

    /// Emits a project's removal; its tasks' sessions' membership follows.
    pub(crate) fn emit_project_removed(&self, st: &mut HubState, project_id: ProjectId) {
        st.emit(StateEvent::ProjectRemoved { project_id });
        self.refresh_membership(st);
    }
}

/// The error while the agent daemon can't be reached.
pub(crate) fn agentd_unavailable() -> RpcError {
    RpcError::new(ErrorData::Timeout, "the agent daemon can't be reached")
}

impl HubState {
    /// Records a change of the state feed and fans it out to its subscriptions.
    pub(crate) fn emit(&mut self, event: StateEvent) {
        let change: StateChange = self.feed.push(event);
        for client in self.clients.values_mut() {
            for (sub, buffer) in client.state_subs.iter_mut() {
                match buffer {
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

    /// Emits a device.
    pub(crate) fn emit_device(&mut self, device: Device) {
        self.emit(StateEvent::DeviceUpserted { device });
    }

    /// Emits a device's removal.
    pub(crate) fn device_removed(&mut self, device_id: &DeviceId) {
        self.emit(StateEvent::DeviceRemoved {
            device_id: device_id.clone(),
        });
    }

    /// Emits a schedule.
    pub(crate) fn emit_schedule(&mut self, schedule: &Schedule) {
        self.emit(StateEvent::ScheduleUpserted {
            schedule: schedule.clone(),
        });
    }

    /// Emits a schedule's removal.
    pub(crate) fn emit_schedule_removed(&mut self, schedule_id: ScheduleId) {
        self.emit(StateEvent::ScheduleRemoved { schedule_id });
    }

    /// Emits a grant's removal.
    pub(crate) fn emit_grant_removed(&mut self, grant_id: GrantId) {
        self.emit(StateEvent::GrantRemoved { grant_id });
    }

    /// Tells the auto-upgrade watchers a machine has no host request in flight anymore.
    pub(crate) fn host_calls_done(&self, machine_id: MachineId) {
        let _ = self.bus.send(BusEvent::HostCallsDone { machine_id });
    }

    /// Every machine as the agent daemon sees it.
    pub(crate) fn host_machines(&self) -> Vec<HostMachine> {
        self.machines
            .values()
            .map(MachineEntry::host_machine)
            .collect()
    }

    /// Whether a machine is due for an automatic upgrade.
    fn upgrade_check(&self, id: MachineId) -> UpgradeCheck {
        self.machines.get(&id).map_or(UpgradeCheck::Off, |e| {
            e.upgrade_check(self.router.in_flight(id))
        })
    }

    /// See [`Hub::disconnected`].
    fn disconnected(&mut self, id: MachineId, connectivity: Connectivity, error: Option<String>) {
        let Some(entry) = self.machines.get_mut(&id) else {
            return;
        };
        if let Some(link) = entry.link.take() {
            link.daemon.close();
        }
        let before = std::mem::replace(&mut entry.live.connectivity, connectivity.clone());
        entry.live.track_since(&before);
        entry.live.bootstrap = None;
        if error.is_some() {
            entry.live.last_error = error;
        }
        self.emit_machine(id);
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
            let params = SessionEventsParams {
                subscription_id: d.subscription_id,
                id: key.session_id,
                head_seq: d.batch.head_seq,
                ops: d.batch.ops,
                raw: d.batch.raw,
                detail: d.batch.detail,
            };
            client.sink.notify::<SessionEvents>(params);
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
