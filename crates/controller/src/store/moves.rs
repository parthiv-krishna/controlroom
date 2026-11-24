//! `profiles.move` in one transaction: machines change membership, projects and their tasks
//! change profile, and the task links and contact grants that would cross profiles go.

use controlroom_protocol::ids::{GrantId, MachineId, ProfileId, ProjectId, TaskId};
use controlroom_protocol::ui::TaskLink;

use super::{OwnerId, Store, StoreResult, machines};

/// Everything one move writes.
#[derive(Debug, Clone)]
pub struct MoveWrite {
    /// The target profile.
    pub profile_id: ProfileId,
    /// Machines with their new profiles.
    pub machines: Vec<(MachineId, Vec<ProfileId>)>,
    /// Projects that move to the target.
    pub project_ids: Vec<ProjectId>,
    /// Tasks that move to the target (the projects' tasks).
    pub task_ids: Vec<TaskId>,
    /// Task links to remove.
    pub unlinked: Vec<TaskLink>,
    /// Contact grants to remove.
    pub revoked_grants: Vec<GrantId>,
}

/// Owner-scoped move operations.
pub struct Moves<'a> {
    store: &'a Store,
    owner: &'a OwnerId,
}

impl<'a> Moves<'a> {
    pub(super) fn new(store: &'a Store, owner: &'a OwnerId) -> Self {
        Self { store, owner }
    }

    /// Applies a move atomically.
    pub fn apply(&self, write: &MoveWrite) -> StoreResult<()> {
        let owner = self.owner.as_str();
        let target = write.profile_id.to_string();
        self.store.transaction(|tx| {
            for (machine, profiles) in &write.machines {
                machines::set_profiles(tx, self.owner, *machine, profiles)?;
            }
            for project in &write.project_ids {
                tx.execute(
                    "UPDATE projects SET profile_id = ?3 WHERE owner_id = ?1 AND id = ?2",
                    (owner, project.to_string(), &target),
                )?;
            }
            for task in &write.task_ids {
                tx.execute(
                    "UPDATE tasks SET profile_id = ?3 WHERE owner_id = ?1 AND id = ?2",
                    (owner, task.to_string(), &target),
                )?;
            }
            for link in &write.unlinked {
                tx.execute(
                    "DELETE FROM task_links WHERE owner_id = ?1 AND task_id = ?2
                     AND session_id = ?3",
                    (owner, link.task_id.to_string(), link.session_id.to_string()),
                )?;
            }
            for grant in &write.revoked_grants {
                tx.execute(
                    "DELETE FROM grants WHERE owner_id = ?1 AND id = ?2",
                    (owner, grant.to_string()),
                )?;
            }
            Ok(())
        })
    }
}
