//! The per-owner scheduler task.
//!
//! Each pass re-reads every enabled schedule under the hub lock (so it never races an edit),
//! applies [`plan::step`], records misses, claims due times, and starts a delivery for each
//! pending due time that isn't being delivered or waiting to retry. Deliveries run
//! concurrently, so a slow machine never holds up another schedule. A delivery that can't
//! reach the machine is retried when the machine connects, or after
//! [`Tuning::schedule_retry`](crate::Tuning), until the due time's deadline.
//!
//! Passes run when a time is due, when a schedule changes ([`Scheduler::wake`]), when a
//! delivery ends, and on bus events that matter: a machine connecting, a session closing or
//! being removed. The first pass after a start applies each schedule's `missed` policy to the
//! due times that passed while the controller was down.
//!
//! Schedules of a closed or removed session, or a removed machine, are disabled.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::{MachineId, ScheduleId};
use controlroom_protocol::session::SessionState;
use controlroom_protocol::ui::{Connectivity, Schedule, ScheduleRun, StateEvent};
use controlroom_protocol::{RpcError, UnixMillis};
use tokio::sync::{Notify, broadcast};
use tokio::task::{JoinHandle, JoinSet};

use super::deliver::{Delivered, deliver};
use super::plan;
use super::recurrence::Recurrence;
use super::runs;
use crate::errors::{not_found, store_error};
use crate::hub::{BusEvent, Hub, HubState};
use crate::store::Timing;

/// The handle the hub keeps: wakes and stops the task.
#[derive(Default)]
pub(crate) struct Scheduler {
    wake: Arc<Notify>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Scheduler {
    /// Starts the task. Call once the hub's machines are registered.
    pub(crate) fn start(&self, hub: &Arc<Hub>) {
        let task = Task {
            hub: Arc::downgrade(hub),
            wake: self.wake.clone(),
            in_flight: HashSet::new(),
            retry_at: HashMap::new(),
            notes: HashMap::new(),
            attempts: JoinSet::new(),
        };
        let bus = hub.subscribe_bus();
        let handle = tokio::spawn(task.run(bus));
        *self.lock() = Some(handle);
    }

    /// Runs a pass soon (a schedule was created, changed or its machine removed).
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    /// Stops the task and its deliveries.
    pub(crate) async fn stop(&self) {
        let task = self.lock().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<JoinHandle<()>>> {
        self.task.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A finished delivery.
struct Attempt {
    id: ScheduleId,
    machine: MachineId,
    at: UnixMillis,
    result: Delivered,
}

struct Task {
    hub: Weak<Hub>,
    wake: Arc<Notify>,
    /// Schedules with a delivery running.
    in_flight: HashSet<ScheduleId>,
    /// After a failed delivery: retry at this time, or when the machine connects.
    retry_at: HashMap<ScheduleId, (MachineId, UnixMillis)>,
    /// Notes for claimed due times, added to their run entry.
    notes: HashMap<(ScheduleId, UnixMillis), String>,
    attempts: JoinSet<Attempt>,
}

impl Task {
    async fn run(mut self, mut bus: broadcast::Receiver<BusEvent>) {
        let Some(clock) = self.hub.upgrade().map(|h| h.services.tuning.clock.clone()) else {
            return;
        };
        let mut first = true;
        let mut dirty = true;
        let mut wake_at = None;
        loop {
            if dirty {
                let Some(hub) = self.hub.upgrade() else {
                    return;
                };
                wake_at = self.pass(&hub, clock.now(), first);
                first = false;
                dirty = false;
            }
            let sleep = async {
                match wake_at {
                    Some(at) => clock.sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = sleep => dirty = true,
                () = self.wake.notified() => dirty = true,
                Some(done) = self.attempts.join_next(), if !self.attempts.is_empty() => {
                    if let (Ok(attempt), Some(hub)) = (done, self.hub.upgrade()) {
                        self.finish(&hub, attempt, clock.now());
                    }
                    dirty = true;
                }
                event = bus.recv() => match event {
                    Ok(event) => dirty |= self.on_bus(event),
                    Err(broadcast::error::RecvError::Lagged(_)) => dirty = true,
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            }
        }
    }

    /// Whether a bus event needs a pass.
    fn on_bus(&mut self, event: BusEvent) -> bool {
        match event {
            BusEvent::Connectivity {
                machine_id,
                connectivity: Connectivity::Connected,
            } => {
                self.retry_at.retain(|_, (m, _)| *m != machine_id);
                true
            }
            BusEvent::Session { current, .. } => current.state == SessionState::Closed,
            BusEvent::SessionRemoved {
                machine_id,
                session_id,
            } => {
                if let Some(hub) = self.hub.upgrade() {
                    let ids = hub
                        .repo()
                        .schedules()
                        .enabled_for_session(machine_id, session_id);
                    match ids {
                        Ok(ids) => ids.into_iter().for_each(|id| disable(&hub, id)),
                        Err(e) => tracing::warn!(error = %e, "reading schedules"),
                    }
                }
                false
            }
            BusEvent::Connectivity { .. } | BusEvent::MachineChanged { .. } => false,
        }
    }

    /// One pass; returns when the next one is due.
    fn pass(&mut self, hub: &Arc<Hub>, now: UnixMillis, first: bool) -> Option<UnixMillis> {
        let schedules = match hub.repo().schedules().list() {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = %e, "reading schedules");
                return Some(retry_time(hub, now));
            }
        };
        let mut wake_at: Option<UnixMillis> = None;
        let mut soonest = |t: Option<UnixMillis>| {
            if let Some(t) = t {
                wake_at = Some(wake_at.map_or(t, |w| w.min(t)));
            }
        };
        for s in schedules {
            if !s.enabled || self.in_flight.contains(&s.id) {
                continue;
            }
            let Some(s) = self.advance(hub, s.id, now, first) else {
                continue;
            };
            soonest(s.next_run);
            let Some(at) = s.pending_run else {
                continue;
            };
            match self.retry_at.get(&s.id) {
                Some((_, retry)) if *retry > now => soonest(Some(*retry)),
                _ => self.start_delivery(hub, s, at),
            }
        }
        wake_at
    }

    /// Re-reads a schedule under the hub lock and applies the plan; returns it, unless it is
    /// no longer enabled.
    fn advance(
        &mut self,
        hub: &Hub,
        id: ScheduleId,
        now: UnixMillis,
        first: bool,
    ) -> Option<Schedule> {
        let mut st = hub.lock();
        let repo = hub.repo();
        let s = repo.schedules().get(id).ok().flatten()?;
        if !s.enabled {
            return None;
        }
        if gone(&st, &s) {
            disable_locked(hub, &mut st, id);
            return None;
        }
        // A spec stored by another build may not parse here: it never fires.
        let r = Recurrence::new(&s.spec, &s.timezone).ok()?;
        let Some(step) = plan::step(
            &r,
            s.next_run,
            s.pending_run,
            now,
            first.then_some(s.missed),
        ) else {
            return Some(s);
        };
        if let Some((at, Some(note))) = &step.claim {
            self.notes.insert((id, *at), note.clone());
        }
        self.retry_at.remove(&id);
        let timing = Timing {
            next_run: step.next_run,
            pending_run: step.pending_run,
        };
        match repo.schedules().advance(id, &step.missed, timing) {
            Ok(Some(s)) => {
                st.emit(StateEvent::ScheduleUpserted {
                    schedule: s.clone(),
                });
                Some(s)
            }
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, "advancing a schedule");
                None
            }
        }
    }

    fn start_delivery(&mut self, hub: &Arc<Hub>, s: Schedule, at: UnixMillis) {
        self.in_flight.insert(s.id);
        let hub = hub.clone();
        self.attempts.spawn(async move {
            let result = deliver(&hub, &s, at).await;
            Attempt {
                id: s.id,
                machine: s.machine_id,
                at,
                result,
            }
        });
    }

    fn finish(&mut self, hub: &Hub, attempt: Attempt, now: UnixMillis) {
        let Attempt {
            id,
            machine,
            at,
            result,
        } = attempt;
        self.in_flight.remove(&id);
        match result {
            Delivered::Retry(e) => {
                tracing::debug!(schedule = %id, error = %e.message, "schedule delivery will be retried");
                self.retry_at.insert(id, (machine, retry_time(hub, now)));
            }
            Delivered::Done {
                outcome,
                detail,
                disable: off,
            } => {
                self.retry_at.remove(&id);
                let note = self.notes.remove(&(id, at));
                let detail = match (note, detail) {
                    (Some(n), Some(d)) => Some(format!("{n}; {d}")),
                    (n, d) => n.or(d),
                };
                record(hub, id, runs::run(at, outcome, detail), Some(at));
                if off {
                    disable(hub, id);
                }
            }
        }
    }
}

fn retry_time(hub: &Hub, now: UnixMillis) -> UnixMillis {
    let retry = i64::try_from(hub.services.tuning.schedule_retry.as_millis()).unwrap_or(i64::MAX);
    UnixMillis(now.0.saturating_add(retry))
}

/// The schedule's machine was removed, or its session is closed.
fn gone(st: &HubState, s: &Schedule) -> bool {
    match st.machines.get(&s.machine_id) {
        None => true,
        Some(entry) => entry
            .sessions
            .get(&s.session_id)
            .is_some_and(|session| session.state == SessionState::Closed),
    }
}

fn disable(hub: &Hub, id: ScheduleId) {
    let mut st = hub.lock();
    disable_locked(hub, &mut st, id);
}

fn disable_locked(hub: &Hub, st: &mut HubState, id: ScheduleId) {
    match hub.repo().schedules().disable(id) {
        Ok(Some(schedule)) => st.emit(StateEvent::ScheduleUpserted { schedule }),
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "disabling a schedule"),
    }
}

/// Records a run and emits the schedule; returns it.
fn record(
    hub: &Hub,
    id: ScheduleId,
    run: ScheduleRun,
    resolves: Option<UnixMillis>,
) -> Option<Schedule> {
    let mut st = hub.lock();
    match hub.repo().schedules().record(id, run, resolves) {
        Ok(Some(schedule)) => {
            st.emit(StateEvent::ScheduleUpserted {
                schedule: schedule.clone(),
            });
            Some(schedule)
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, "recording a schedule run");
            None
        }
    }
}

/// `schedules.run_now`: delivers the message now (same busy rule), as due time "now". An
/// unreachable machine is an error and records nothing.
pub(crate) async fn run_now(hub: &Hub, id: ScheduleId) -> Result<Schedule, RpcError> {
    let schedule = hub
        .repo()
        .schedules()
        .get(id)
        .map_err(store_error)?
        .ok_or_else(|| not_found(Resource::Schedule, id))?;
    let at = hub.services.tuning.clock.now();
    match deliver(hub, &schedule, at).await {
        Delivered::Retry(e) => Err(e),
        Delivered::Done {
            outcome,
            detail,
            disable: off,
        } => {
            let detail = Some(detail.map_or("run now".to_owned(), |d| format!("run now; {d}")));
            record(hub, id, runs::run(at, outcome, detail), None);
            if off {
                disable(hub, id);
            }
            hub.repo()
                .schedules()
                .get(id)
                .map_err(store_error)?
                .ok_or_else(|| not_found(Resource::Schedule, id))
        }
    }
}
