//! Server-owned attention: which session changes deserve a notification.
//!
//! The agent daemon computes each session's `attention` and pending `requests` (spec §13: the
//! controller and UI derive nothing). [`detect`] compares two summaries of one session and
//! returns the occurrences that are new, each with a key that identifies it within the session.
//! Delivery deduplicates on that key, so "at most one push per device per (session, attention
//! event)" holds across restarts. [`AttentionEvent::still_pending`] tells whether a later
//! summary still shows the occurrence, so a delayed push is dropped once it was dealt with.

use controlroom_protocol::Seq;
use controlroom_protocol::ids::RequestId;
use controlroom_protocol::session::{
    Attention, Request, RequestState, RunOutcome, SessionState, SessionSummary,
};

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
}

impl AttentionKind {
    /// The `state` shown in a push.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            AttentionKind::Approval => "approval",
            AttentionKind::Question => "question",
            AttentionKind::Problem => "problem",
            AttentionKind::TurnEnd => "finished",
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
        }
    }

    /// Whether `current` (a later summary of the same session) still shows the occurrence: the
    /// request is still pending, the problem still stands after the same turn, or the finished
    /// run is still unread and no other turn ended since.
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
/// With no previous summary (first observation, e.g. after a controller restart), only pending
/// requests fire: they may have arrived while nobody watched, while old failures and finished
/// runs must not alert again. Repeats are absorbed by delivery dedup.
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

    fn keys(events: &[AttentionEvent]) -> Vec<String> {
        events.iter().map(|e| e.event_key()).collect()
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
