//! Schedules (`schedules.*`, spec §8.1): data only in this phase. The scheduler that fires
//! them (and fills `next_run` and `recent_runs`) arrives in P5, so `run_now` is `unsupported`
//! and the `schedules` capability isn't advertised yet.

use controlroom_protocol::error::Resource;
use controlroom_protocol::ui::{
    Schedule, ScheduleRef, ScheduleSpec, SchedulesCreateParams, SchedulesUpdateParams, StateEvent,
};
use controlroom_protocol::{Ack, ErrorData, RpcError};

use super::Conn;
use crate::errors::{invalid_params, not_found, store_error};
use crate::store::NewSchedule;

fn check_spec(spec: &ScheduleSpec) -> Result<(), RpcError> {
    match spec {
        ScheduleSpec::Cron { expr } if expr.split_whitespace().count() == 5 => Ok(()),
        ScheduleSpec::Cron { .. } => Err(invalid_params("cron expressions have five fields")),
        ScheduleSpec::Interval { every_s } if *every_s >= 60 => Ok(()),
        ScheduleSpec::Interval { .. } => Err(invalid_params("intervals are at least 60 s")),
        ScheduleSpec::Unknown => Err(invalid_params("unknown schedule spec")),
    }
}

fn get(conn: &Conn, id: controlroom_protocol::ids::ScheduleId) -> Result<Schedule, RpcError> {
    conn.hub
        .repo()
        .schedules()
        .get(id)
        .map_err(store_error)?
        .ok_or_else(|| not_found(Resource::Schedule, id))
}

pub(super) fn create(conn: &Conn, p: SchedulesCreateParams) -> Result<Schedule, RpcError> {
    check_spec(&p.spec)?;
    let mut st = conn.hub.lock();
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
            enabled: p.enabled.unwrap_or(true),
            if_busy: p.if_busy.unwrap_or_default(),
            missed: p.missed.unwrap_or_default(),
        })
        .map_err(store_error)?;
    st.emit(StateEvent::ScheduleUpserted {
        schedule: schedule.clone(),
    });
    Ok(schedule)
}

pub(super) fn update(conn: &Conn, p: SchedulesUpdateParams) -> Result<Schedule, RpcError> {
    if let Some(spec) = &p.spec {
        check_spec(spec)?;
    }
    let mut st = conn.hub.lock();
    if !conn
        .hub
        .repo()
        .schedules()
        .update(&p)
        .map_err(store_error)?
    {
        return Err(not_found(Resource::Schedule, p.schedule_id));
    }
    let schedule = get(conn, p.schedule_id)?;
    st.emit(StateEvent::ScheduleUpserted {
        schedule: schedule.clone(),
    });
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

pub(super) fn run_now(conn: &Conn, p: ScheduleRef) -> Result<Schedule, RpcError> {
    get(conn, p.schedule_id)?;
    Err(RpcError::new(
        ErrorData::Unsupported,
        "schedules don't fire yet (the scheduler arrives in a later release)",
    ))
}
