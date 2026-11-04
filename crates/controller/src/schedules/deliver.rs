//! Delivering one due time: `session.prompt` with the schedule's message, `origin: schedule`,
//! its busy policy, and the deterministic op id `<schedule id>:<scheduled unix ms>`, to the agent
//! daemon. The agent daemon stores op ids with their effect, so delivering the same due time again (a retry, or after a
//! controller restart) returns the first result instead of prompting twice.

use controlroom_client_core::daemon::CallError;
use controlroom_protocol::agentd::{PromptResult, SessionPromptParams, methods};
use controlroom_protocol::ids::OpId;
use controlroom_protocol::session::{ContentPart, PromptOrigin, SessionState};
use controlroom_protocol::ui::{Schedule, ScheduleOutcome};
use controlroom_protocol::{ErrorData, RpcError, UnixMillis};

use crate::hub::Hub;

/// How a delivery ended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Delivered {
    /// The agent daemon answered: the outcome and a detail for the run entry.
    Done {
        outcome: ScheduleOutcome,
        detail: Option<String>,
        /// The session is closed or gone: disable the schedule.
        disable: bool,
    },
    /// The agent daemon couldn't be reached (or asked to retry later); try again until the
    /// deadline.
    Retry(RpcError),
}

/// The op id of a due time.
pub(crate) fn op_id(schedule: &Schedule, at: UnixMillis) -> OpId {
    OpId(format!("{}:{}", schedule.id, at.0))
}

/// Sends the prompt for due time `at`.
pub(crate) async fn deliver(hub: &Hub, schedule: &Schedule, at: UnixMillis) -> Delivered {
    let state = hub.session(schedule.session_id).map(|s| s.state);
    let daemon = match hub.agentd() {
        Ok(daemon) => daemon,
        Err(e) => return Delivered::Retry(e),
    };
    let busy = state.is_some_and(is_busy);
    let params = SessionPromptParams {
        op_id: op_id(schedule, at),
        id: schedule.session_id,
        content: vec![ContentPart::Text {
            text: schedule.message.clone(),
        }],
        origin: PromptOrigin::Schedule {
            schedule_id: schedule.id,
            name: schedule.name.clone(),
        },
        if_busy: schedule.if_busy,
    };
    match daemon.call::<methods::SessionPrompt>(&params).await {
        Ok(result) => answered(result, busy),
        Err(CallError::Rpc(e)) => refused(e),
        Err(e) => Delivered::Retry(crate::rpc::agentd_error(e)),
    }
}

/// A run is active, so a queued prompt waits behind it.
fn is_busy(state: SessionState) -> bool {
    matches!(
        state,
        SessionState::Working | SessionState::AwaitingPermission | SessionState::AwaitingInput
    )
}

fn answered(result: PromptResult, busy: bool) -> Delivered {
    let outcome = match result {
        // A dormant or idle session's prompt is queued while the agent starts: it still starts
        // a run.
        PromptResult::Queued { .. } if busy => ScheduleOutcome::Queued,
        PromptResult::Skipped => ScheduleOutcome::Skipped,
        PromptResult::Sent
        | PromptResult::Steered
        | PromptResult::Queued { .. }
        | PromptResult::Unknown => ScheduleOutcome::Fired,
    };
    let detail = (outcome == ScheduleOutcome::Skipped).then(|| "the session was busy".to_owned());
    Delivered::Done {
        outcome,
        detail,
        disable: false,
    }
}

fn refused(e: RpcError) -> Delivered {
    let done = |outcome, detail: &str, disable| Delivered::Done {
        outcome,
        detail: Some(detail.to_owned()),
        disable,
    };
    match &e.data {
        // The op id was used with other params: this due time was delivered before the
        // schedule changed (the controller stopped before recording it).
        Some(ErrorData::OpIdConflict) => done(ScheduleOutcome::Fired, "sent earlier", false),
        Some(ErrorData::Busy | ErrorData::Timeout) => Delivered::Retry(e),
        Some(ErrorData::InvalidState {
            state: SessionState::Closed,
        }) => done(ScheduleOutcome::Failed, "the session is closed", true),
        Some(ErrorData::UnknownSession { .. } | ErrorData::Deleted { .. }) => done(
            ScheduleOutcome::Failed,
            "the session no longer exists",
            true,
        ),
        _ => done(ScheduleOutcome::Failed, &e.message, false),
    }
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::ids::{QueueId, SessionId};

    use super::*;

    fn outcome(d: Delivered) -> (ScheduleOutcome, bool) {
        match d {
            Delivered::Done {
                outcome, disable, ..
            } => (outcome, disable),
            Delivered::Retry(e) => panic!("retry: {e:?}"),
        }
    }

    #[test]
    fn results_map_to_outcomes() {
        let queued = PromptResult::Queued {
            queue_id: QueueId::generate(),
        };
        let fired = (ScheduleOutcome::Fired, false);
        assert_eq!(outcome(answered(PromptResult::Sent, false)), fired);
        assert_eq!(outcome(answered(queued.clone(), false)), fired);
        assert_eq!(
            outcome(answered(queued, true)),
            (ScheduleOutcome::Queued, false)
        );
        assert_eq!(
            outcome(answered(PromptResult::Skipped, true)),
            (ScheduleOutcome::Skipped, false)
        );
    }

    #[test]
    fn refusals_fail_retry_or_disable() {
        let err = |data| RpcError::new(data, "no");
        let closed = ErrorData::InvalidState {
            state: SessionState::Closed,
        };
        assert_eq!(
            outcome(refused(err(closed))),
            (ScheduleOutcome::Failed, true)
        );
        let gone = ErrorData::UnknownSession {
            id: SessionId::generate(),
        };
        assert_eq!(outcome(refused(err(gone))), (ScheduleOutcome::Failed, true));
        assert_eq!(
            outcome(refused(err(ErrorData::OpIdConflict))),
            (ScheduleOutcome::Fired, false)
        );
        assert_eq!(
            outcome(refused(err(ErrorData::NotResumable))),
            (ScheduleOutcome::Failed, false)
        );
        assert!(matches!(refused(err(ErrorData::Busy)), Delivered::Retry(_)));
    }
}
