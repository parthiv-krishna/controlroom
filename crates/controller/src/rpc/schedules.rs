//! Schedules (`schedules.*`, spec §8.1). Edits validate the spec and time zone and reset the
//! timing when the spec, zone or `enabled` changes; the scheduler (`crate::schedules`) fires
//! them. A schedule can't be enabled on a session that is known to be closed, nor created on
//! an unknown machine.
//!
//! Every write happens under the hub lock, like the scheduler's, so edits and the scheduler
//! never interleave.

use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::{MachineId, ScheduleId, SessionId};
use controlroom_protocol::session::SessionState;
use controlroom_protocol::ui::{
    Schedule, ScheduleRef, ScheduleSpec, SchedulesCreateParams, SchedulesUpdateParams, StateEvent,
};
use controlroom_protocol::{Ack, RpcError};

use super::Conn;
use crate::errors::{invalid_params, not_found, store_error};
use crate::hub::HubState;
use crate::schedules::{Recurrence, run_now as fire_now};
use crate::store::{NewSchedule, Timing};

fn recurrence(spec: &ScheduleSpec, tz: &str) -> Result<Recurrence, RpcError> {
    Recurrence::new(spec, tz).map_err(|e| invalid_params(e.to_string()))
}

fn check_text(name: &str, message: &str) -> Result<(), RpcError> {
    if name.trim().is_empty() {
        return Err(invalid_params("the schedule needs a name"));
    }
    if message.trim().is_empty() {
        return Err(invalid_params("the schedule needs a message"));
    }
    Ok(())
}

/// Refuses enabling a schedule on a closed session (closing disables them).
fn check_open(st: &HubState, machine: MachineId, session: SessionId) -> Result<(), RpcError> {
    let closed = st
        .machines
        .get(&machine)
        .and_then(|m| m.sessions.get(&session))
        .is_some_and(|s| s.state == SessionState::Closed);
    if closed {
        return Err(invalid_params(
            "the session is closed; reopen it to enable its schedules",
        ));
    }
    Ok(())
}

fn get(conn: &Conn, id: ScheduleId) -> Result<Schedule, RpcError> {
    conn.hub
        .repo()
        .schedules()
        .get(id)
        .map_err(store_error)?
        .ok_or_else(|| not_found(Resource::Schedule, id))
}

pub(super) fn create(conn: &Conn, p: SchedulesCreateParams) -> Result<Schedule, RpcError> {
    let r = recurrence(&p.spec, &p.timezone)?;
    check_text(&p.name, &p.message)?;
    let enabled = p.enabled.unwrap_or(true);
    let now = conn.hub.services.tuning.clock.now();
    let schedule = {
        let mut st = conn.hub.lock();
        if !st.machines.contains_key(&p.machine_id) {
            return Err(not_found(Resource::Machine, p.machine_id));
        }
        if enabled {
            check_open(&st, p.machine_id, p.session_id)?;
        }
        let schedule = conn
            .hub
            .repo()
            .schedules()
            .insert(NewSchedule {
                machine_id: p.machine_id,
                session_id: p.session_id,
                name: p.name,
                spec: p.spec,
                timezone: p.timezone,
                message: p.message,
                enabled,
                if_busy: p.if_busy.unwrap_or_default(),
                missed: p.missed.unwrap_or_default(),
                next_run: if enabled { r.first_after(now) } else { None },
            })
            .map_err(store_error)?;
        st.emit(StateEvent::ScheduleUpserted {
            schedule: schedule.clone(),
        });
        schedule
    };
    conn.hub.scheduler.wake();
    Ok(schedule)
}

pub(super) fn update(conn: &Conn, p: SchedulesUpdateParams) -> Result<Schedule, RpcError> {
    let now = conn.hub.services.tuning.clock.now();
    let schedule = {
        let mut st = conn.hub.lock();
        let old = get(conn, p.schedule_id)?;
        let spec = p.spec.as_ref().unwrap_or(&old.spec);
        let timezone = p.timezone.as_deref().unwrap_or(&old.timezone);
        let r = recurrence(spec, timezone)?;
        check_text(
            p.name.as_deref().unwrap_or(&old.name),
            p.message.as_deref().unwrap_or(&old.message),
        )?;
        let enabled = p.enabled.unwrap_or(old.enabled);
        if enabled && !old.enabled {
            check_open(&st, old.machine_id, old.session_id)?;
        }
        let retimed = *spec != old.spec || timezone != old.timezone || enabled != old.enabled;
        let timing = retimed.then(|| Timing {
            next_run: if enabled { r.first_after(now) } else { None },
            pending_run: None,
        });
        if !conn
            .hub
            .repo()
            .schedules()
            .update(&p, timing)
            .map_err(store_error)?
        {
            return Err(not_found(Resource::Schedule, p.schedule_id));
        }
        let schedule = get(conn, p.schedule_id)?;
        st.emit(StateEvent::ScheduleUpserted {
            schedule: schedule.clone(),
        });
        schedule
    };
    conn.hub.scheduler.wake();
    Ok(schedule)
}

pub(super) fn delete(conn: &Conn, p: ScheduleRef) -> Result<Ack, RpcError> {
    let mut st = conn.hub.lock();
    if !conn
        .hub
        .repo()
        .schedules()
        .delete(p.schedule_id)
        .map_err(store_error)?
    {
        return Err(not_found(Resource::Schedule, p.schedule_id));
    }
    st.emit(StateEvent::ScheduleRemoved {
        schedule_id: p.schedule_id,
    });
    Ok(Ack {})
}

/// Sends the message now and records the outcome; an unreachable machine is an error.
pub(super) async fn run_now(conn: &Conn, p: ScheduleRef) -> Result<Schedule, RpcError> {
    fire_now(&conn.hub, p.schedule_id).await
}
