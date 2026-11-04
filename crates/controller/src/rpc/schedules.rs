//! Schedules (`schedules.*`, spec §8.1) for both UI APIs. Edits validate the spec and time
//! zone and reset the timing when the spec, zone or `enabled` changes; the scheduler
//! (`crate::schedules`) fires them. A schedule can't be enabled on a session that is known to
//! be closed. The old UI API's schedules wake a legacy daemon's session (created on a known
//! machine); the next API's wake a global session. Each API only sees and edits its own.
//!
//! Every write happens under the hub lock, like the scheduler's, so edits and the scheduler
//! never interleave.

use controlroom_protocol::daemon::IfBusy;
use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::{MachineId, ScheduleId, SessionId};
use controlroom_protocol::session::SessionState;
use controlroom_protocol::ui::{MissedPolicy, ScheduleRef, ScheduleSpec, SchedulesUpdateParams};
use controlroom_protocol::{Ack, RpcError};

use super::{Conn, UiApi};
use crate::errors::{invalid_params, not_found, store_error};
use crate::hub::HubState;
use crate::schedules::{Recurrence, run_now as fire_now};
use crate::store::{NewSchedule, ScheduleRow, Timing};

/// A schedule to create, from either API's params.
pub(super) struct Create {
    pub machine_id: Option<MachineId>,
    pub session_id: SessionId,
    pub name: String,
    pub spec: ScheduleSpec,
    pub timezone: String,
    pub message: String,
    pub enabled: Option<bool>,
    pub if_busy: Option<IfBusy>,
    pub missed: Option<MissedPolicy>,
}

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
fn check_open(
    st: &HubState,
    machine: Option<MachineId>,
    session: SessionId,
) -> Result<(), RpcError> {
    let state = match machine {
        Some(machine) => st
            .machines
            .get(&machine)
            .and_then(|m| m.sessions.get(&session))
            .map(|s| s.state),
        None => st.global.sessions.get(&session).map(|s| s.state),
    };
    if state == Some(SessionState::Closed) {
        return Err(invalid_params(
            "the session is closed; reopen it to enable its schedules",
        ));
    }
    Ok(())
}

/// A schedule of `api`, or `not_found`.
fn get(conn: &Conn, id: ScheduleId, api: UiApi) -> Result<ScheduleRow, RpcError> {
    conn.hub
        .repo()
        .schedules()
        .get(id)
        .map_err(store_error)?
        .filter(|s| s.machine_id.is_some() == (api == UiApi::Legacy))
        .ok_or_else(|| not_found(Resource::Schedule, id))
}

pub(super) fn create(conn: &Conn, p: Create) -> Result<ScheduleRow, RpcError> {
    let r = recurrence(&p.spec, &p.timezone)?;
    check_text(&p.name, &p.message)?;
    let enabled = p.enabled.unwrap_or(true);
    let now = conn.hub.services.tuning.clock.now();
    let schedule = {
        let mut st = conn.hub.lock();
        if let Some(machine) = p.machine_id
            && !st.machines.contains_key(&machine)
        {
            return Err(not_found(Resource::Machine, machine));
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
        st.emit_schedule(&schedule);
        schedule
    };
    conn.hub.scheduler.wake();
    Ok(schedule)
}

pub(super) fn update(
    conn: &Conn,
    p: SchedulesUpdateParams,
    api: UiApi,
) -> Result<ScheduleRow, RpcError> {
    let now = conn.hub.services.tuning.clock.now();
    let schedule = {
        let mut st = conn.hub.lock();
        let old = get(conn, p.schedule_id, api)?;
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
        let schedule = get(conn, p.schedule_id, api)?;
        st.emit_schedule(&schedule);
        schedule
    };
    conn.hub.scheduler.wake();
    Ok(schedule)
}

pub(super) fn delete(conn: &Conn, p: ScheduleRef, api: UiApi) -> Result<Ack, RpcError> {
    let mut st = conn.hub.lock();
    get(conn, p.schedule_id, api)?;
    if !conn
        .hub
        .repo()
        .schedules()
        .delete(p.schedule_id)
        .map_err(store_error)?
    {
        return Err(not_found(Resource::Schedule, p.schedule_id));
    }
    st.emit_schedule_removed(p.schedule_id);
    Ok(Ack {})
}

/// Sends the message now and records the outcome; an unreachable daemon is an error.
pub(super) async fn run_now(
    conn: &Conn,
    p: ScheduleRef,
    api: UiApi,
) -> Result<ScheduleRow, RpcError> {
    get(conn, p.schedule_id, api)?;
    fire_now(&conn.hub, p.schedule_id).await
}
