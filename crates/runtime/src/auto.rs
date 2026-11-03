//! Auto-continue (spec §8): when the agent ends its turn and open todo entries remain (pending
//! or in progress), the daemon starts a continue run, unless
//!
//! - a question is open (its answer will wake the agent instead);
//! - background work is running (its wake will wake the agent instead: see
//!   [`crate::session::Session::live_work`]);
//! - an approval is pending, the run failed or was cancelled, or the queue (or an injection)
//!   has items: they go first;
//! - auto-continue is off for the session.
//!
//! It pauses with attention when only blocked entries remain, after
//! [`AutoContinue::STALL_RUNS`] continue runs that closed no entry (stalled), when the run
//! budget is used up, and when the user cancels a run. Turning it on (or resuming it) starts a
//! fresh budget; so does every run the user starts.
//!
//! The decision is taken by the reconcile loop once the agent is idle after a run settled
//! ([`Due::Settled`], set by the fold) or after the user resumed it ([`Due::Resumed`]). A
//! continuation is an injection with origin `continue`, so it is delivered and tagged like any
//! other, and its run's trigger is `auto_continue`.

use controlroom_drivers::LineTag;
use controlroom_protocol::RpcError;
use controlroom_protocol::daemon::SessionUpdateParams;
use controlroom_protocol::ids::SessionId;
use controlroom_protocol::session::{AutoContinueInfo, AutoContinueStatus, RunOutcome, TodoStatus};
use controlroom_protocol::timeline::{InjectOrigin, RunTrigger};

use crate::core::Core;
use crate::model::{AutoContinue, Due, Injection, Pause, SessionData};
use crate::session::Session;
use crate::todos::render_list;

impl Core {
    /// Starts a continue run when one is due and allowed (see the module docs). Called by the
    /// reconcile loop after the queue and the injections.
    pub(crate) fn auto_continue(&mut self, id: SessionId) {
        let session = &self.sessions[&id];
        let Some(due) = session.data.auto.due else {
            return;
        };
        if !session.view().ready || session.busy() {
            // Undecided until the agent is idle; a run that settles decides again.
            return;
        }
        let decision = decide(session, due);
        let now = self.clock.now();
        let Ok(session) = self.session_mut(id) else {
            return;
        };
        let auto = &mut session.data.auto;
        auto.due = None;
        match decision {
            Decision::Wait => {}
            Decision::Pause(pause, reason) => auto.pause = Some((pause, reason)),
            Decision::Continue => {
                auto.runs_used += 1;
                let text = continue_prompt(&session.data);
                let injection = Injection::new(
                    InjectOrigin::Continue,
                    "auto-continue".into(),
                    text,
                    true,
                    now,
                );
                if let Err(e) = self.inject(id, injection) {
                    tracing::warn!("session {id}: auto-continue: {e}");
                }
            }
        }
    }

    /// `session.update`'s auto-continue settings: turning it on resumes a paused auto-continue
    /// with a fresh budget; a budget above the runs used resumes a budget pause.
    pub(crate) fn update_auto_continue(
        &mut self,
        params: &SessionUpdateParams,
    ) -> Result<(), RpcError> {
        let auto = &mut self.session_mut(params.id)?.data.auto;
        if let Some(budget) = params.auto_continue_budget {
            auto.budget = budget;
            if matches!(auto.pause, Some((Pause::Budget, _))) && auto.runs_used < budget {
                auto.pause = None;
                auto.due = Some(Due::Resumed);
            }
        }
        match params.auto_continue {
            Some(true) if !auto.enabled || auto.pause.is_some() => {
                auto.enabled = true;
                auto.pause = None;
                auto.runs_used = 0;
                auto.stalls = 0;
                auto.due = Some(Due::Resumed);
            }
            Some(false) => {
                auto.enabled = false;
                auto.pause = None;
                auto.due = None;
            }
            Some(true) | None => {}
        }
        Ok(())
    }

    /// The user cancelled a run: auto-continue pauses (while there is something to continue).
    pub(crate) fn auto_continue_cancelled(&mut self, id: SessionId) -> Result<(), RpcError> {
        let data = &mut self.session_mut(id)?.data;
        if data.auto.enabled && data.auto.pause.is_none() && data.open_todos() > 0 {
            data.auto.pause = Some((Pause::Paused, "Cancelled by the user".into()));
        }
        Ok(())
    }
}

/// Fold reactions: a run started (by `cause`) or settled.
pub(crate) fn run_started(data: &mut SessionData, cause: Option<&LineTag>) {
    let continuing = cause
        .and_then(|tag| data.trigger_for(tag))
        .is_some_and(|t| *t == RunTrigger::AutoContinue);
    let auto = &mut data.auto;
    auto.continuing = continuing;
    auto.progress = false;
    // A run the user started (directly or from the queue) is a new instruction.
    if matches!(cause, Some(LineTag::Op { .. } | LineTag::Queue { .. })) {
        auto.runs_used = 0;
        auto.stalls = 0;
    }
}

/// See [`run_started`].
pub(crate) fn run_settled(data: &mut SessionData) {
    let auto = &mut data.auto;
    if auto.progress {
        auto.stalls = 0;
    } else if auto.continuing {
        auto.stalls += 1;
    }
    auto.continuing = false;
    auto.progress = false;
    auto.due = Some(Due::Settled);
}

/// A todo entry changed status: closing one is progress.
pub(crate) fn todo_status_changed(auto: &mut AutoContinue, status: TodoStatus) {
    if matches!(
        status,
        TodoStatus::Done | TodoStatus::Blocked | TodoStatus::Skipped
    ) {
        auto.progress = true;
    }
}

enum Decision {
    /// Something else will wake the agent, or nothing is to be done.
    Wait,
    Pause(Pause, String),
    Continue,
}

fn decide(session: &Session, due: Due) -> Decision {
    let data = &session.data;
    let auto = &data.auto;
    if !auto.enabled || auto.pause.is_some() || data.closed() {
        return Decision::Wait;
    }
    if data.open_todos() == 0 {
        return if data.todos.iter().any(|t| t.status == TodoStatus::Blocked) {
            Decision::Pause(Pause::Paused, "Only blocked items remain".into())
        } else {
            Decision::Wait
        };
    }
    if due == Due::Settled && data.last_outcome != Some(RunOutcome::Completed) {
        return Decision::Wait;
    }
    if waiting_on(session).is_some() {
        return Decision::Wait;
    }
    if auto.stalls >= AutoContinue::STALL_RUNS {
        let reason = format!("{} runs without progress", auto.stalls);
        return Decision::Pause(Pause::Stalled, reason);
    }
    if auto.runs_used >= auto.budget {
        let reason = format!("Budget of {} runs used", auto.budget);
        return Decision::Pause(Pause::Budget, reason);
    }
    Decision::Continue
}

/// What will wake the agent instead of a continuation, if anything.
pub(crate) fn waiting_on(session: &Session) -> Option<String> {
    let pending = session.pending_requests();
    if pending.iter().any(|r| r.blocking) {
        return Some("waiting on an approval".into());
    }
    if !pending.is_empty() {
        return Some("waiting on an answer".into());
    }
    let work = session.live_work();
    if work.jobs > 0 {
        return Some("waiting on job".into());
    }
    if work.children > 0 {
        return Some("waiting on a subagent".into());
    }
    if work.asks > 0 {
        return Some("waiting on a reply".into());
    }
    if !session.data.queue.is_empty() {
        return Some("queued prompts go first".into());
    }
    if session.data.injections.iter().any(|i| i.wake) {
        return Some("a wake is being delivered".into());
    }
    None
}

/// The auto-continue details for the session detail and the agent.
pub(crate) fn info(session: &Session) -> AutoContinueInfo {
    let auto = &session.data.auto;
    let status = auto.status();
    let reason = match (&auto.pause, status) {
        (Some((_, reason)), _) => Some(reason.clone()),
        (None, AutoContinueStatus::On) if session.data.open_todos() > 0 => waiting_on(session),
        _ => None,
    };
    AutoContinueInfo {
        status,
        reason,
        runs_used: auto.runs_used,
        budget: auto.budget,
    }
}

/// The prompt of a continue run: the reason and the open entries.
fn continue_prompt(data: &SessionData) -> String {
    format!(
        "[controlroom] Auto-continue: open todo items remain, so please continue with the next \
         one. Mark items done (or blocked with a reason) as you go with the todo tool; if you \
         can't make progress without the user, ask with ask_user or block the item.\n\n{}",
        render_list(&data.todos)
    )
}
