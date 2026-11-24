//! A session's contact directory (architecture §10): the sessions it may reach without asking,
//! namely its parent and children, the sessions linked to a task it is linked to, and pairs the
//! user granted. Contact never crosses profiles (C5): a peer of another profile, or one whose
//! profile isn't known, is left out, whatever the relation.

use std::collections::{BTreeMap, HashSet};

use controlroom_protocol::ids::{DaemonId, ProfileId, SessionId};
use controlroom_protocol::session::{Peer, PeerRef, PeerRelation, SessionSummary};
use controlroom_protocol::ui::{ContactGrant, Task};

/// The directory of `session`, each peer once (by its closest relation: parent, child, task,
/// grant). `sessions` are every session of the agent daemon `agentd`; `profile_of` places a
/// session in its profile (`HubState::session_profile`).
pub(crate) fn directory(
    sessions: &BTreeMap<SessionId, SessionSummary>,
    profile_of: impl Fn(&SessionSummary) -> Option<ProfileId>,
    tasks: &[Task],
    grants: &[ContactGrant],
    agentd: DaemonId,
    session: SessionId,
) -> Vec<Peer> {
    let Some(me) = sessions.get(&session) else {
        return Vec::new();
    };
    let Some(profile) = profile_of(me) else {
        return Vec::new();
    };
    let mut candidates: Vec<(SessionId, PeerRelation, Option<String>)> = Vec::new();
    if let Some(parent) = me.parent_id {
        candidates.push((parent, PeerRelation::Parent, None));
    }
    candidates.extend(
        sessions
            .values()
            .filter(|s| s.parent_id == Some(session))
            .map(|s| (s.id, PeerRelation::Child, None)),
    );
    for task in tasks.iter().filter(|t| t.links.contains(&session)) {
        candidates.extend(
            task.links
                .iter()
                .map(|peer| (*peer, PeerRelation::Task, Some(task.title.clone()))),
        );
    }
    for grant in grants {
        let peer = if grant.from == session {
            grant.to
        } else if grant.to == session {
            grant.from
        } else {
            continue;
        };
        candidates.push((peer, PeerRelation::Grant, None));
    }

    let mut seen = HashSet::from([session]);
    candidates
        .into_iter()
        .filter_map(|(id, relation, task)| {
            let peer = sessions.get(&id)?;
            if profile_of(peer) != Some(profile) || !seen.insert(id) {
                return None;
            }
            Some(Peer {
                peer: PeerRef {
                    daemon_id: agentd,
                    session_id: id,
                    name: peer
                        .name
                        .clone()
                        .or_else(|| peer.title.clone())
                        .unwrap_or_default(),
                },
                relation,
                task,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::UnixMillis;
    use controlroom_protocol::ids::{GrantId, TaskId};
    use controlroom_protocol::ui::{ContactScope, Priority, TaskStatus};

    use super::*;
    use crate::attention::tests::summary;

    fn task(title: &str, profile: ProfileId, links: &[SessionId]) -> Task {
        Task {
            id: TaskId::generate(),
            title: title.into(),
            status: TaskStatus::Doing,
            order: 0.0,
            priority: Priority::P2,
            project_ids: vec![],
            profile_id: Some(profile),
            notes: String::new(),
            checklist: vec![],
            links: links.to_vec(),
            created_at: UnixMillis(0),
            updated_at: UnixMillis(0),
        }
    }

    fn grant(from: SessionId, to: SessionId) -> ContactGrant {
        ContactGrant {
            id: GrantId::generate(),
            from,
            to,
            scope: ContactScope::Pair,
            granted_at: UnixMillis(0),
            by: None,
        }
    }

    /// Every relation reaches a peer of the same profile; none reaches another profile's
    /// session or one without a known profile.
    #[test]
    fn the_directory_never_crosses_profiles() {
        let (garden, books) = (ProfileId::generate(), ProfileId::generate());
        let mut sessions = BTreeMap::new();
        let mut add = |name: &str, profile: Option<ProfileId>, parent: Option<SessionId>| {
            let mut s = summary();
            s.id = SessionId::generate();
            s.name = Some(name.into());
            s.profile_id = profile;
            s.parent_id = parent;
            sessions.insert(s.id, s.clone());
            s.id
        };
        let me = add("planner", Some(garden), None);
        add("child", Some(garden), Some(me));
        let linked = add("linked", Some(garden), None);
        let granted = add("granted", Some(garden), None);
        add("other child", Some(books), Some(me));
        let other_linked = add("other linked", Some(books), None);
        let other_granted = add("other granted", Some(books), None);
        let unplaced = add("unplaced", None, None);
        let tasks = [task("seeds", garden, &[me, linked, other_linked, unplaced])];
        let grants = [
            grant(me, granted),
            grant(other_granted, me),
            grant(linked, granted),
        ];

        let agentd = DaemonId::generate();
        let peers = directory(&sessions, |s| s.profile_id, &tasks, &grants, agentd, me);
        let found: Vec<_> = peers
            .iter()
            .map(|p| (p.peer.name.as_str(), p.relation, p.task.as_deref()))
            .collect();
        assert_eq!(
            found,
            [
                ("child", PeerRelation::Child, None),
                ("linked", PeerRelation::Task, Some("seeds")),
                ("granted", PeerRelation::Grant, None),
            ]
        );
        assert!(peers.iter().all(|p| p.peer.daemon_id == agentd));

        // A session without a known profile reaches nobody.
        assert!(
            directory(
                &sessions,
                |s| s.profile_id,
                &tasks,
                &grants,
                agentd,
                unplaced
            )
            .is_empty()
        );
    }
}
