//! Global sessions: the agent daemon's sessions feed (`sessions.subscribe`) applied to the hub's
//! cache, each session's project membership (the projects of the tasks that link it), and the
//! workspace overlaps. Everything here feeds the UI state feed.
//!
//! The cache survives a lost link to the agent daemon (sessions never go stale; their
//! workspaces carry their own availability) and is replaced by the next snapshot.

use std::collections::{BTreeMap, HashMap, HashSet};

use controlroom_protocol::RemotePath;
use controlroom_protocol::Rev;
use controlroom_protocol::agentd::{
    SessionsChange, SessionsEvent, SessionsEventsParams, SessionsSubscribeResult,
};
use controlroom_protocol::ids::{MachineId, ProjectId, SessionId, SubscriptionId};
use controlroom_protocol::session::{SessionState, SessionSummary};
use controlroom_protocol::ui::{FeedSession, Overlap, Project, StateEvent, Task};

use super::{BusEvent, Hub, HubState};

/// The hub's view of the agent daemon's sessions.
#[derive(Debug, Default)]
pub(crate) struct GlobalSessions {
    /// Every session, by id.
    pub sessions: BTreeMap<SessionId, SessionSummary>,
    /// Each linked session's projects, in project order; absent means Unassigned.
    pub membership: HashMap<SessionId, Vec<ProjectId>>,
    /// Directories shared by several open sessions.
    pub overlaps: Vec<Overlap>,
    /// The current `sessions.subscribe` subscription.
    pub sub: Option<SubscriptionId>,
    /// The last revision applied; resumes the feed after a reconnect.
    pub rev: Option<Rev>,
}

impl GlobalSessions {
    /// A session with its project membership.
    pub(crate) fn feed_session(&self, session: &SessionSummary) -> FeedSession {
        FeedSession {
            project_ids: self
                .membership
                .get(&session.id)
                .cloned()
                .unwrap_or_default(),
            session: session.clone(),
        }
    }

    /// Every session with its membership, oldest first.
    pub(crate) fn feed(&self) -> Vec<FeedSession> {
        let mut sessions: Vec<_> = self
            .sessions
            .values()
            .map(|s| self.feed_session(s))
            .collect();
        sessions.sort_by_key(|s| (s.session.created_at, s.session.id));
        sessions
    }
}

/// Each session's projects: the union of the projects of the tasks that link it, in project
/// order.
pub(crate) fn membership(
    tasks: &[Task],
    projects: &[Project],
) -> HashMap<SessionId, Vec<ProjectId>> {
    let mut linked: HashMap<SessionId, HashSet<ProjectId>> = HashMap::new();
    for task in tasks {
        for session in &task.links {
            linked
                .entry(*session)
                .or_default()
                .extend(task.project_ids.iter().copied());
        }
    }
    linked
        .into_iter()
        .filter_map(|(session, set)| {
            let ordered: Vec<_> = projects
                .iter()
                .map(|p| p.id)
                .filter(|id| set.contains(id))
                .collect();
            (!ordered.is_empty()).then_some((session, ordered))
        })
        .collect()
}

/// The directories used by several open sessions' workspaces: by machine and resolved root
/// (the path as given until the host resolved it). Sessions oldest first.
pub(crate) fn overlaps<'a>(sessions: impl Iterator<Item = &'a SessionSummary>) -> Vec<Overlap> {
    let mut by_dir: BTreeMap<(MachineId, RemotePath), Vec<&SessionSummary>> = BTreeMap::new();
    for session in sessions.filter(|s| s.state != SessionState::Closed) {
        let mut seen = HashSet::new();
        for w in &session.workspaces {
            let dir = w.root.clone().unwrap_or_else(|| w.path.clone());
            if seen.insert((w.machine_id, dir.clone())) {
                by_dir.entry((w.machine_id, dir)).or_default().push(session);
            }
        }
    }
    by_dir
        .into_iter()
        .filter(|(_, users)| users.len() > 1)
        .map(|((machine_id, path), mut users)| {
            users.sort_by_key(|s| (s.created_at, s.id));
            Overlap {
                machine_id,
                path,
                session_ids: users.into_iter().map(|s| s.id).collect(),
            }
        })
        .collect()
}

impl HubState {
    /// Stores a summary; emits it (and the overlaps) if it changed.
    fn global_upsert(&mut self, session: SessionSummary) {
        let previous = self.global.sessions.insert(session.id, session.clone());
        if previous.as_ref() == Some(&session) {
            return;
        }
        let _ = self.bus.send(BusEvent::Session {
            previous: previous.map(Box::new),
            current: Box::new(session.clone()),
        });
        let feed = self.global.feed_session(&session);
        self.emit(StateEvent::SessionUpserted {
            session: Box::new(feed),
        });
    }

    fn global_remove(&mut self, id: SessionId) {
        if self.global.sessions.remove(&id).is_none() {
            return;
        }
        let _ = self.bus.send(BusEvent::SessionRemoved { session_id: id });
        self.emit(StateEvent::SessionRemoved { session_id: id });
    }

    /// Emits the overlaps if they changed.
    fn refresh_overlaps(&mut self) {
        let now = overlaps(self.global.sessions.values());
        if now != self.global.overlaps {
            self.global.overlaps = now.clone();
            self.emit(StateEvent::OverlapsChanged { overlaps: now });
        }
    }

    /// Applies a `sessions.subscribe` result (from its response hook, before any of its
    /// events).
    pub(crate) fn sessions_subscribed(&mut self, result: SessionsSubscribeResult) {
        self.global.sub = Some(result.subscription_id);
        self.global.rev = Some(result.rev);
        if let Some(snapshot) = result.snapshot {
            let keep: HashSet<_> = snapshot.sessions.iter().map(|s| s.id).collect();
            let gone: Vec<_> = self
                .global
                .sessions
                .keys()
                .filter(|id| !keep.contains(id))
                .copied()
                .collect();
            for id in gone {
                self.global_remove(id);
            }
            for session in snapshot.sessions {
                self.global_upsert(session);
            }
        }
        self.refresh_overlaps();
    }

    /// Applies `sessions.events` of the current subscription.
    pub(crate) fn sessions_events(&mut self, p: SessionsEventsParams) {
        if self.global.sub != Some(p.subscription_id) {
            return;
        }
        for SessionsChange { rev, event } in p.changes {
            self.global.rev = Some(rev);
            match event {
                SessionsEvent::SessionUpserted { session } => self.global_upsert(*session),
                SessionsEvent::SessionRemoved { id } => self.global_remove(id),
                SessionsEvent::SettingsChanged { .. } | SessionsEvent::Unknown => {}
            }
        }
        self.refresh_overlaps();
    }
}

impl Hub {
    /// Recomputes every session's project membership after a task or project change, and
    /// emits the sessions whose membership changed.
    pub(crate) fn refresh_membership(&self, st: &mut HubState) {
        let repo = self.repo();
        let (tasks, projects) = match (repo.tasks().list(), repo.projects().list()) {
            (Ok(tasks), Ok(projects)) => (tasks, projects),
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!(error = %e, "reading tasks for project membership");
                return;
            }
        };
        let now = membership(&tasks, &projects);
        let changed: Vec<SessionId> = st
            .global
            .sessions
            .keys()
            .filter(|id| now.get(id) != st.global.membership.get(id))
            .copied()
            .collect();
        st.global.membership = now;
        for id in changed {
            if let Some(session) = st.global.sessions.get(&id) {
                let feed = st.global.feed_session(session);
                st.emit(StateEvent::SessionUpserted {
                    session: Box::new(feed),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::UnixMillis;
    use controlroom_protocol::ids::{TaskId, WorkspaceId};
    use controlroom_protocol::session::{Availability, Workspace};
    use controlroom_protocol::ui::{Connectivity, Priority, TaskStatus};
    use serde_json::json;

    use super::*;

    fn session(created: i64, dirs: &[(MachineId, &str, Option<&str>)]) -> SessionSummary {
        let mut s: SessionSummary = serde_json::from_value(json!({
            "id": SessionId::generate(), "name": null, "title": null, "workspaces": [],
            "shells": [], "created_at": created, "updated_at": 0, "closed_at": null,
            "parent_id": null, "state": "idle", "last_outcome": null, "last_error": null,
            "read_seq": 0, "last_turn_end_seq": 0, "unread": false, "preview": "",
            "attention": null, "rollup": {"approvals": 0, "questions": 0, "problems": 0, "unread": 0},
            "context": null,
            "queue": {"len": 0, "paused": false, "auto_send": true, "send_now_pending": false},
            "requests": [], "todo": null,
            "live_work": {"jobs": 0, "children": 0, "asks": 0, "host_calls": 0},
            "options": [], "agent_live": true,
            "sandbox": {"level": "strict", "default_level": "strict", "explicit": false,
                        "looser_than_default": false, "grants": []},
            "resumable": true
        }))
        .unwrap();
        s.workspaces = dirs
            .iter()
            .enumerate()
            .map(|(i, (machine_id, path, root))| Workspace {
                id: WorkspaceId::generate(),
                machine_id: *machine_id,
                path: (*path).into(),
                root: root.map(Into::into),
                primary: i == 0,
                added_at: UnixMillis(0),
                availability: Availability {
                    connectivity: Connectivity::Connected,
                    since: UnixMillis(0),
                },
            })
            .collect();
        s
    }

    #[test]
    fn overlaps_group_open_sessions_by_machine_and_root() {
        let (devbox, buildbox) = (MachineId::generate(), MachineId::generate());
        let a = session(2, &[(devbox, "~/src", Some("/home/alice/src"))]);
        let b = session(
            1,
            &[
                (devbox, "/home/alice/src", Some("/home/alice/src")),
                (buildbox, "~/src", None),
            ],
        );
        let c = session(3, &[(buildbox, "~/src", None)]);
        let mut closed = session(0, &[(devbox, "~/src", Some("/home/alice/src"))]);
        closed.state = SessionState::Closed;
        let alone = session(4, &[(devbox, "~/other", None)]);
        let found = overlaps([&a, &b, &c, &closed, &alone].into_iter());
        let mut expected = vec![
            Overlap {
                machine_id: devbox,
                path: "/home/alice/src".into(),
                session_ids: vec![b.id, a.id],
            },
            Overlap {
                machine_id: buildbox,
                path: "~/src".into(),
                session_ids: vec![b.id, c.id],
            },
        ];
        expected.sort_by(|x, y| (x.machine_id, &x.path).cmp(&(y.machine_id, &y.path)));
        assert_eq!(found, expected);
    }

    #[test]
    fn membership_follows_links_in_project_order() {
        let projects: Vec<Project> = ["web", "api", "ops"]
            .iter()
            .map(|name| Project {
                id: ProjectId::generate(),
                name: (*name).into(),
                color: "#000".into(),
            })
            .collect();
        let (s1, s2) = (SessionId::generate(), SessionId::generate());
        let task = |projects: Vec<ProjectId>, links: Vec<SessionId>| Task {
            id: TaskId::generate(),
            title: "t".into(),
            status: TaskStatus::Doing,
            order: 0.0,
            priority: Priority::P2,
            project_ids: projects,
            notes: String::new(),
            checklist: Vec::new(),
            links,
            created_at: UnixMillis(0),
            updated_at: UnixMillis(0),
        };
        let tasks = [
            task(vec![projects[2].id], vec![s1]),
            task(vec![projects[0].id, projects[2].id], vec![s1]),
            task(vec![], vec![s2]),
        ];
        let m = membership(&tasks, &projects);
        assert_eq!(m.get(&s1), Some(&vec![projects[0].id, projects[2].id]));
        assert_eq!(
            m.get(&s2),
            None,
            "a task without projects leaves it Unassigned"
        );
    }
}
