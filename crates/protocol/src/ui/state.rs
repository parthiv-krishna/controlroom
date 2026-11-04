//! The aggregated state feed (`state.subscribe`): machines, the agent daemon's global sessions
//! with their project membership, workspace overlaps, and the controller's own data, behind
//! one epoch+rev cursor.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{
    DeviceId, Epoch, GrantId, MachineId, ProjectId, RemotePath, Rev, ScheduleId, SessionId,
    SubscriptionId, TaskId,
};
use crate::session::SessionSummary;
use crate::ui::{ContactGrant, Device, Machine, Project, Schedule, Task};

/// A session in the state feed: the agent daemon's summary plus its project membership.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct FeedSession {
    /// The projects of the tasks that link the session, in project order; empty means
    /// "Unassigned".
    pub project_ids: Vec<ProjectId>,
    /// The agent daemon's summary.
    #[serde(flatten)]
    pub session: SessionSummary,
}

/// A directory used by several open sessions' workspaces at once. Allowed, but the UI warns:
/// their edits can collide (each write is compare-and-swap, so a collision is a `conflict`,
/// never a lost update).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Overlap {
    /// The machine.
    pub machine_id: MachineId,
    /// The shared directory (the workspaces' resolved root).
    pub path: RemotePath,
    /// The sessions sharing it (at least two), oldest first.
    pub session_ids: Vec<SessionId>,
}

/// A position in the state feed. A new `epoch` (the controller restarted) invalidates every
/// `rev`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct StateCursor {
    /// The feed's lifetime.
    pub epoch: Epoch,
    /// The revision within it.
    pub rev: Rev,
}

/// Params of `state.subscribe`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct StateSubscribeParams {
    /// The last cursor the client applied, or `null` for a fresh start.
    pub since: Option<StateCursor>,
}

/// Everything the state feed describes. Contact requests are the `contact` requests in the
/// sessions' `requests`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct StateSnapshot {
    /// Every machine.
    pub machines: Vec<Machine>,
    /// Every session.
    pub sessions: Vec<FeedSession>,
    /// Directories shared by several open sessions.
    pub overlaps: Vec<Overlap>,
    /// Every project.
    pub projects: Vec<Project>,
    /// Every task.
    pub tasks: Vec<Task>,
    /// Every contact grant.
    pub grants: Vec<ContactGrant>,
    /// Every schedule.
    pub schedules: Vec<Schedule>,
    /// Every device of the user.
    pub devices: Vec<Device>,
}

/// Result of `state.subscribe`. With a `snapshot` (fresh start, another epoch, or a cursor too
/// old), the snapshot replaces the client's state and `state.events` follow from `cursor`.
/// Without one, `state.events` follow with every change after `since`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct StateSubscribeResult {
    /// Identifies this subscription.
    pub subscription_id: SubscriptionId,
    /// The current cursor.
    pub cursor: StateCursor,
    /// The full state at `cursor`, when needed.
    pub snapshot: Option<StateSnapshot>,
}

/// One change in the state feed, tagged by `kind`. Upserts carry the whole object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StateEvent {
    /// A machine was added or changed. Sessions with a workspace on it are upserted too when
    /// their workspace availability changes.
    MachineUpserted {
        /// The machine.
        machine: Box<Machine>,
    },
    /// A machine was removed (its sessions stay; their workspaces there become unavailable).
    MachineRemoved {
        /// The machine.
        machine_id: MachineId,
    },
    /// A session appeared or changed, or its project membership changed.
    SessionUpserted {
        /// The session.
        session: Box<FeedSession>,
    },
    /// A session was deleted.
    SessionRemoved {
        /// The session.
        session_id: SessionId,
    },
    /// The overlapping directories changed; replaces the whole list.
    OverlapsChanged {
        /// The new list.
        overlaps: Vec<Overlap>,
    },
    /// A project was created or changed.
    ProjectUpserted {
        /// The project.
        project: Project,
    },
    /// A project was deleted.
    ProjectRemoved {
        /// The project.
        project_id: ProjectId,
    },
    /// A task was created or changed.
    TaskUpserted {
        /// The task.
        task: Task,
    },
    /// A task was deleted.
    TaskRemoved {
        /// The task.
        task_id: TaskId,
    },
    /// A contact grant was created.
    GrantUpserted {
        /// The grant.
        grant: ContactGrant,
    },
    /// A contact grant was revoked or used up.
    GrantRemoved {
        /// The grant.
        grant_id: GrantId,
    },
    /// A schedule was created or changed (including its runs).
    ScheduleUpserted {
        /// The schedule.
        schedule: Schedule,
    },
    /// A schedule was deleted.
    ScheduleRemoved {
        /// The schedule.
        schedule_id: ScheduleId,
    },
    /// A device was registered or changed.
    DeviceUpserted {
        /// The device.
        device: Device,
    },
    /// A device was removed. If it is this browser, it has been signed out.
    DeviceRemoved {
        /// The device.
        device_id: DeviceId,
    },
    /// An event this build doesn't know; ignore it (the rev still counts).
    #[serde(other)]
    Unknown,
}

/// A [`StateEvent`] with its revision; on the wire the event's fields sit next to `rev`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct StateChange {
    /// The revision within the current epoch.
    pub rev: Rev,
    /// What changed.
    #[serde(flatten)]
    pub event: StateEvent,
}

/// Params of the `state.events` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct StateEventsParams {
    /// The subscription.
    pub subscription_id: SubscriptionId,
    /// Changes in `rev` order.
    pub changes: Vec<StateChange>,
}
