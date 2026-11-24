//! Server-owned attention: which session changes deserve a notification.
//!
//! The agent daemon computes each session's `attention` and pending `requests` (spec §13: the
//! controller and UI derive nothing). [`detect`] compares two summaries of one session and
//! returns the occurrences that are new, each with a key that identifies it within the session.
//! Delivery deduplicates on that key, so "at most one push per device per (session, attention
//! event)" holds across restarts. [`AttentionEvent::still_pending`] tells whether a later
//! summary still shows the occurrence, so a delayed push is dropped once it was dealt with.

use controlroom_protocol::Seq;
use controlroom_protocol::ids::{ItemId, RequestId};
use controlroom_protocol::session::{
    Attention, OpenNotice, Request, RequestState, RunOutcome, SessionState, SessionSummary,
};
use controlroom_protocol::timeline::NoticeLevel;
use controlroom_protocol::ui::PushState;

/// What a notification is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AttentionKind {
    /// A blocking approval or dialog is pending.
    Approval,
    /// A non-blocking question is open (the lighter tier).
    Question,
    /// The last run failed or was interrupted, or auto-continue paused.
    Problem,
    /// A run finished and its output is unread.
    TurnEnd,
    /// The agent's warning or error notice is open.
    Notice(NoticeLevel),
}

impl AttentionKind {
    /// The `state` shown in a push.
    pub(crate) fn push_state(self) -> PushState {
        match self {
            AttentionKind::Approval => PushState::Approval,
            AttentionKind::Question => PushState::Question,
            AttentionKind::Problem => PushState::Problem,
            AttentionKind::TurnEnd => PushState::Finished,
            AttentionKind::Notice(_) => PushState::Notice,
        }
    }
}

/// What an occurrence is about within its session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Occurrence {
    /// A request (approvals and questions).
    Request(RequestId),
    /// The turn that ended at this seq (problems and finished runs).
    TurnEnd(Seq),
    /// An agent notice, by its timeline item.
    Notice(ItemId),
}

/// One notification-worthy occurrence in a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AttentionEvent {
    pub kind: AttentionKind,
    pub occurrence: Occurrence,
}

impl AttentionEvent {
    /// Identifies the occurrence within the session, e.g. `approval:<request id>` (the
    /// delivery dedup key).
    pub(crate) fn event_key(&self) -> String {
        match (&self.occurrence, self.kind) {
            (Occurrence::Request(id), AttentionKind::Approval) => format!("approval:{id}"),
            (Occurrence::Request(id), _) => format!("question:{id}"),
            (Occurrence::TurnEnd(seq), AttentionKind::Problem) => format!("problem@{}", seq.0),
            (Occurrence::TurnEnd(seq), _) => format!("turn_end@{}", seq.0),
            (Occurrence::Notice(id), _) => format!("notice:{id}"),
        }
    }

    /// Whether `current` (a later summary of the same session) still shows the occurrence: the
    /// request is still pending, the problem still stands after the same turn, the finished run
    /// is still unread and no other turn ended since, or the notice is still open.
    pub(crate) fn still_pending(&self, current: &SessionSummary) -> bool {
        match &self.occurrence {
            Occurrence::Request(id) => current
                .requests
                .iter()
                .any(|r| r.id == *id && r.state == RequestState::Pending),
            Occurrence::TurnEnd(seq) => {
                current.last_turn_end_seq == *seq
                    && match self.kind {
                        AttentionKind::Problem => current.attention == Some(Attention::Problem),
                        _ => current.unread,
                    }
            }
            Occurrence::Notice(id) => own_notices(current).any(|n| n.item_id == *id),
        }
    }
}

/// The session's own pending requests of one tier (descendants' roll up from their own
/// summaries).
fn pending(session: &SessionSummary, blocking: bool) -> impl Iterator<Item = &Request> {
    session.requests.iter().filter(move |r| {
        r.state == RequestState::Pending && r.blocking == blocking && r.origin_child.is_none()
    })
}

/// The session's own open notices (descendants' roll up from their own summaries).
fn own_notices(session: &SessionSummary) -> impl Iterator<Item = &OpenNotice> {
    session.notices.iter().filter(|n| n.origin_child.is_none())
}

/// The newest request of `current` in a tier that `previous` didn't have pending.
fn new_request<'a>(
    previous: Option<&SessionSummary>,
    current: &'a SessionSummary,
    blocking: bool,
) -> Option<&'a Request> {
    pending(current, blocking)
        .filter(|r| previous.is_none_or(|p| !pending(p, blocking).any(|old| old.id == r.id)))
        .max_by_key(|r| r.created_at)
}

/// The occurrences that are new in `current` compared with `previous`.
///
/// Every new open notice fires (its level decides whether it pushes).
///
/// With no previous summary (first observation, e.g. after a controller restart), only pending
/// requests and open notices fire: they may have arrived while nobody watched, while old
/// failures and finished runs must not alert again. Repeats are absorbed by delivery dedup.
pub(crate) fn detect(
    previous: Option<&SessionSummary>,
    current: &SessionSummary,
) -> Vec<AttentionEvent> {
    let mut events = Vec::new();
    for (blocking, kind) in [
        (true, AttentionKind::Approval),
        (false, AttentionKind::Question),
    ] {
        if let Some(request) = new_request(previous, current, blocking) {
            events.push(AttentionEvent {
                kind,
                occurrence: Occurrence::Request(request.id.clone()),
            });
        }
    }
    for notice in own_notices(current) {
        if previous.is_some_and(|p| own_notices(p).any(|old| old.item_id == notice.item_id)) {
            continue;
        }
        events.push(AttentionEvent {
            kind: AttentionKind::Notice(notice.level),
            occurrence: Occurrence::Notice(notice.item_id.clone()),
        });
    }
    let Some(previous) = previous else {
        return events;
    };
    let turn_ended = current.last_turn_end_seq > previous.last_turn_end_seq;
    let at_turn_end = Occurrence::TurnEnd(current.last_turn_end_seq);
    if current.attention == Some(Attention::Problem)
        && (previous.attention != Some(Attention::Problem) || turn_ended)
    {
        events.push(AttentionEvent {
            kind: AttentionKind::Problem,
            occurrence: at_turn_end,
        });
    } else if turn_ended
        && current.unread
        && current.last_outcome == Some(RunOutcome::Completed)
        && !matches!(
            current.state,
            SessionState::Working | SessionState::Starting
        )
    {
        events.push(AttentionEvent {
            kind: AttentionKind::TurnEnd,
            occurrence: at_turn_end,
        });
    }
    events
}

#[cfg(test)]
pub(crate) mod tests {
    use controlroom_protocol::UnixMillis;
    use controlroom_protocol::session::RequestKind;
    use serde_json::json;

    use super::*;

    /// An idle session with nothing pending.
    pub(crate) fn summary() -> SessionSummary {
        serde_json::from_value(json!({
            "id": "01HZY3K6W8Q9X2V4N7B5C1D3E6", "name": null, "title": null, "workspaces": [],
            "shells": [], "created_at": 0, "updated_at": 0, "closed_at": null,
            "parent_id": null, "state": "idle", "last_outcome": null,
            "last_error": null, "read_seq": 0, "last_turn_end_seq": 0, "unread": false,
            "preview": "", "attention": null,
            "rollup": {"approvals": 0, "questions": 0, "problems": 0, "unread": 0},
            "context": null,
            "queue": {"len": 0, "paused": false, "auto_send": true, "send_now_pending": false},
            "requests": [], "todo": null, "live_work": {"jobs": 0, "children": 0, "asks": 0},
            "options": [], "agent_live": true,
            "sandbox": {"level": "strict", "default_level": "strict", "explicit": false,
                        "looser_than_default": false, "grants": []},
            "resumable": true
        }))
        .unwrap()
    }

    /// A pending request (an approval if `blocking`, else a question).
    pub(crate) fn request(id: &str, blocking: bool) -> Request {
        Request {
            id: RequestId::new(id),
            kind: if blocking {
                RequestKind::Permission
            } else {
                RequestKind::Question
            },
            blocking,
            state: RequestState::Pending,
            title: "Run rm?".into(),
            detail: None,
            body: None,
            options: vec![],
            questions: vec![],
            answer: None,
            origin_child: None,
            created_at: UnixMillis(id.len() as i64),
        }
    }

    /// An open agent notice.
    pub(crate) fn notice(id: &str, level: NoticeLevel) -> OpenNotice {
        OpenNotice {
            item_id: ItemId::new(id),
            level,
            text: "devbox has a failed unit".into(),
            created_at: UnixMillis(id.len() as i64),
            origin_child: None,
        }
    }

    fn keys(events: &[AttentionEvent]) -> Vec<String> {
        events.iter().map(|e| e.event_key()).collect()
    }

    #[test]
    fn every_new_open_notice_fires_until_dismissed() {
        let idle = summary();
        let mut warned = summary();
        warned.notices = vec![notice("n1", NoticeLevel::Warning)];
        let events = detect(Some(&idle), &warned);
        assert_eq!(keys(&events), ["notice:n1"]);
        assert_eq!(events[0].kind, AttentionKind::Notice(NoticeLevel::Warning));
        assert!(detect(Some(&warned), &warned).is_empty(), "unchanged");
        // Each new one fires; a child's rolls up from its own summary.
        let mut more = warned.clone();
        more.notices.push(notice("n22", NoticeLevel::Error));
        more.notices.push(notice("n333", NoticeLevel::Warning));
        let mut child = notice("c1", NoticeLevel::Error);
        child.origin_child = Some(idle.id);
        more.notices.push(child);
        assert_eq!(
            keys(&detect(Some(&warned), &more)),
            ["notice:n22", "notice:n333"]
        );
        // A first observation (after a restart) fires the open ones again.
        assert_eq!(keys(&detect(None, &warned)), ["notice:n1"]);

        let [event] = detect(Some(&idle), &warned).try_into().unwrap();
        assert!(event.still_pending(&more));
        let mut dismissed = more.clone();
        dismissed.notices.remove(0);
        assert!(!event.still_pending(&dismissed));
        assert!(detect(Some(&more), &dismissed).is_empty());
    }

    #[test]
    fn new_requests_fire_once_per_request() {
        let idle = summary();
        let mut asking = summary();
        asking.state = SessionState::AwaitingPermission;
        asking.requests = vec![request("r1", true)];
        assert_eq!(keys(&detect(Some(&idle), &asking)), ["approval:r1"]);
        assert!(detect(Some(&asking), &asking).is_empty(), "unchanged");
        // A second request fires; answering the first doesn't.
        let mut two = asking.clone();
        two.requests.push(request("r22", true));
        assert_eq!(keys(&detect(Some(&asking), &two)), ["approval:r22"]);
        let mut answered = two.clone();
        answered.requests.remove(0);
        assert!(detect(Some(&two), &answered).is_empty());
        // Questions are their own tier; rolled-up child requests belong to the child.
        let mut question = idle.clone();
        question.requests = vec![request("q1", false)];
        let mut child = request("c1", true);
        child.origin_child = Some(idle.id);
        question.requests.push(child);
        assert_eq!(keys(&detect(Some(&idle), &question)), ["question:q1"]);
    }

    #[test]
    fn first_observation_fires_only_for_pending_requests() {
        let mut failed = summary();
        failed.state = SessionState::Failed;
        failed.attention = Some(Attention::Problem);
        failed.last_turn_end_seq = Seq(9);
        assert!(detect(None, &failed).is_empty());
        failed.requests = vec![request("r1", true)];
        assert_eq!(keys(&detect(None, &failed)), ["approval:r1"]);
    }

    #[test]
    fn problems_and_finished_runs_fire_on_new_turn_ends() {
        let mut working = summary();
        working.state = SessionState::Working;
        working.last_turn_end_seq = Seq(4);
        let mut failed = working.clone();
        failed.state = SessionState::Failed;
        failed.attention = Some(Attention::Problem);
        failed.last_turn_end_seq = Seq(10);
        assert_eq!(keys(&detect(Some(&working), &failed)), ["problem@10"]);
        assert!(detect(Some(&failed), &failed).is_empty());

        let mut done = working.clone();
        done.state = SessionState::Idle;
        done.unread = true;
        done.attention = Some(Attention::Unread);
        done.last_outcome = Some(RunOutcome::Completed);
        done.last_turn_end_seq = Seq(12);
        let events = detect(Some(&working), &done);
        assert_eq!(keys(&events), ["turn_end@12"]);
        assert_eq!(events[0].kind, AttentionKind::TurnEnd);
        let mut read = done.clone();
        read.unread = false;
        assert!(detect(Some(&working), &read).is_empty());
    }

    #[test]
    fn occurrences_stay_pending_until_dealt_with() {
        let idle = summary();
        let mut asking = summary();
        asking.requests = vec![request("r1", true)];
        let [approval] = detect(Some(&idle), &asking).try_into().unwrap();
        assert!(approval.still_pending(&asking));
        let mut answered = asking.clone();
        answered.requests[0].state = RequestState::Answered;
        assert!(!approval.still_pending(&answered));
        assert!(!approval.still_pending(&idle), "withdrawn");

        let mut failed = summary();
        failed.attention = Some(Attention::Problem);
        failed.last_turn_end_seq = Seq(10);
        let problem = AttentionEvent {
            kind: AttentionKind::Problem,
            occurrence: Occurrence::TurnEnd(Seq(10)),
        };
        assert!(
            problem.still_pending(&failed),
            "reading doesn't clear a problem"
        );
        let mut resumed = failed.clone();
        resumed.attention = None;
        assert!(!problem.still_pending(&resumed));

        let mut done = summary();
        done.unread = true;
        done.last_turn_end_seq = Seq(12);
        let finished = AttentionEvent {
            kind: AttentionKind::TurnEnd,
            occurrence: Occurrence::TurnEnd(Seq(12)),
        };
        assert!(finished.still_pending(&done));
        let mut read = done.clone();
        read.unread = false;
        assert!(!finished.still_pending(&read));
        let mut next = done.clone();
        next.last_turn_end_seq = Seq(14);
        assert!(!finished.still_pending(&next), "a later turn ended");
    }
}
