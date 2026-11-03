//! Runs, retries, compaction, session info and the process lifecycle.

use controlroom_protocol::session::{ContextUsage, RequestState, RunOutcome};
use controlroom_protocol::timeline::{
    CompactionPhase, CompactionReason, ItemBody, RetryPhase, RunState, RunTrigger, ToolStatus,
};
use controlroom_transcript::ExitInfo;

use super::PiDriver;
use super::items::{At, Life, item_id};
use super::state::{Process, Run, SentCommand};
use super::wire::CompactionResult;
use crate::{DriverEvent, FoldOut};

impl PiDriver {
    /// `agent_start`: a new run, or a retry attempt inside the active one.
    pub(super) fn on_agent_start(&mut self, at: At, out: &mut FoldOut) {
        if self.process.run.is_some() {
            if let Some(retry) = self.process.retry.clone() {
                self.items.update(&mut out.ops, &retry, at, |body| {
                    if let ItemBody::Retry { phase, .. } = body {
                        *phase = RetryPhase::Retrying;
                    }
                });
            }
            return;
        }
        let cause = self.process.cause.take();
        let (tag, trigger) = cause.map_or((None, RunTrigger::Unknown), |c| (c.tag, c.trigger));
        let item = item_id("run", at.seq, None);
        self.items.create(
            &mut out.ops,
            item.clone(),
            at,
            at.seq,
            ItemBody::Run {
                trigger,
                state: RunState::Running,
                outcome: None,
                error: None,
            },
            Life::Open,
        );
        self.process.run = Some(Run {
            item,
            last_stop: None,
            retry_error: None,
        });
        self.view.run_active = true;
        out.events
            .push(DriverEvent::RunStarted { cause: tag.clone() });
        // The `started` response came first; its line counts as accepted now that the run is
        // active, so the session never looks idle in between.
        if let Some(tag) = tag
            && self.view.unanswered.contains(&tag)
        {
            self.view.unanswered.retain(|t| *t != tag);
            out.events.push(DriverEvent::Accepted { tag });
        }
    }

    /// `agent_settled`: the run ends; its outcome comes from `aborted`, then the last
    /// assistant `stopReason` (fixtures README obs. 1).
    pub(super) fn on_settled(&mut self, at: At, aborted: bool, out: &mut FoldOut) {
        let Some(run) = self.process.run.take() else {
            return;
        };
        self.end_message(at, out);
        self.close_tools(at, out);
        self.close_retry(at, None, out);
        // Dialogs raised during the run were awaited by it; pi resolved them somehow (abort
        // signal, timeout) without telling us.
        self.close_dialogs(at, RequestState::Withdrawn, |dialog| dialog.in_run, out);
        self.process.echoes.retain(|echo| !echo.started);
        let (outcome, error) = outcome(aborted, &run);
        self.finish_run(at, &run.item, outcome, error.clone(), out);
        out.events.push(DriverEvent::RunSettled { outcome, error });
    }

    fn finish_run(
        &mut self,
        at: At,
        item: &str,
        outcome: RunOutcome,
        error: Option<String>,
        out: &mut FoldOut,
    ) {
        self.items.finish(&mut out.ops, item, at, |body| {
            if let ItemBody::Run {
                state,
                outcome: o,
                error: e,
                ..
            } = body
            {
                *state = RunState::Settled;
                *o = Some(outcome);
                *e = error;
            }
        });
        self.view.run_active = false;
        self.view.last_outcome = Some(outcome);
    }

    /// `auto_retry_start`: opens or advances the retry item.
    pub(super) fn on_retry_start(
        &mut self,
        at: At,
        attempt: u32,
        max_attempts: Option<u32>,
        delay_ms: Option<u64>,
        error: Option<String>,
        out: &mut FoldOut,
    ) {
        let body = ItemBody::Retry {
            attempt,
            max_attempts,
            phase: RetryPhase::Waiting,
            delay_ms,
            error,
        };
        match self.process.retry.clone() {
            Some(item) => {
                self.items.update(&mut out.ops, &item, at, |b| *b = body);
            }
            None => {
                let item = item_id("retry", at.seq, None);
                self.items
                    .create(&mut out.ops, item.clone(), at, at.seq, body, Life::Open);
                self.process.retry = Some(item);
            }
        }
    }

    /// `auto_retry_end`: the retry succeeded or gave up.
    pub(super) fn on_retry_end(
        &mut self,
        at: At,
        success: bool,
        attempt: u32,
        final_error: Option<String>,
        out: &mut FoldOut,
    ) {
        if !success && let Some(run) = &mut self.process.run {
            run.retry_error.clone_from(&final_error);
        }
        if self.process.retry.is_none() {
            self.on_retry_start(at, attempt, None, None, final_error.clone(), out);
        }
        let phase = if success {
            RetryPhase::Succeeded
        } else {
            RetryPhase::GaveUp
        };
        self.close_retry(at, Some((phase, final_error)), out);
    }

    /// Finishes the open retry item: with the given phase, else as given up.
    fn close_retry(
        &mut self,
        at: At,
        end: Option<(RetryPhase, Option<String>)>,
        out: &mut FoldOut,
    ) {
        let Some(item) = self.process.retry.take() else {
            return;
        };
        let (phase, final_error) = end.unwrap_or((RetryPhase::GaveUp, None));
        self.items.finish(&mut out.ops, &item, at, |body| {
            if let ItemBody::Retry {
                phase: p, error, ..
            } = body
            {
                *p = phase;
                if final_error.is_some() {
                    *error = final_error;
                }
            }
        });
    }

    /// `compaction_start`.
    pub(super) fn on_compaction_start(&mut self, at: At, reason: &str, out: &mut FoldOut) {
        let reason = match reason {
            "manual" => CompactionReason::Manual,
            "threshold" => CompactionReason::Threshold,
            "overflow" => CompactionReason::Overflow,
            _ => CompactionReason::Unknown,
        };
        let item = item_id("compaction", at.seq, None);
        self.items.create(
            &mut out.ops,
            item.clone(),
            at,
            at.seq,
            ItemBody::Compaction {
                reason,
                phase: CompactionPhase::Running,
                tokens_before: None,
                tokens_after: None,
                error: None,
            },
            Life::Open,
        );
        self.process.compaction = Some(item);
    }

    /// `compaction_end`: done or failed; a success updates the context.
    pub(super) fn on_compaction_end(
        &mut self,
        at: At,
        result: Option<CompactionResult>,
        aborted: bool,
        error: Option<String>,
        out: &mut FoldOut,
    ) {
        if self.process.compaction.is_none() {
            self.on_compaction_start(at, "", out);
        }
        let error = error.or_else(|| aborted.then(|| "Compaction aborted".to_owned()));
        let result = result.unwrap_or_default();
        let tokens_after = result.estimated_tokens_after;
        self.close_compaction(at, result.tokens_before, tokens_after, error.clone(), out);
        if error.is_none()
            && let Some(used) = tokens_after
        {
            out.events.push(DriverEvent::Context(ContextUsage {
                used,
                size: self.options.context_window(None, None),
                cost: Some(self.cost),
            }));
        }
    }

    fn close_compaction(
        &mut self,
        at: At,
        before: Option<u64>,
        after: Option<u64>,
        error: Option<String>,
        out: &mut FoldOut,
    ) {
        let Some(item) = self.process.compaction.take() else {
            return;
        };
        self.items.finish(&mut out.ops, &item, at, |body| {
            if let ItemBody::Compaction {
                phase,
                tokens_before,
                tokens_after,
                error: e,
                ..
            } = body
            {
                *phase = if error.is_some() {
                    CompactionPhase::Failed
                } else {
                    CompactionPhase::Done
                };
                *tokens_before = before;
                *tokens_after = after;
                *e = error;
            }
        });
    }

    /// `session_info_changed`: the session's title.
    pub(super) fn on_session_name(&mut self, at: At, name: Option<String>, out: &mut FoldOut) {
        self.session_info(at, name.clone(), None, None, out);
        if let Some(name) = name
            && self.title.as_ref() != Some(&name)
        {
            self.title = Some(name.clone());
            out.events.push(DriverEvent::Title(name));
        }
    }

    /// `thinking_level_changed`.
    pub(super) fn on_thinking_level(&mut self, at: At, level: String, out: &mut FoldOut) {
        self.session_info(at, None, None, Some(level.clone()), out);
        self.options.thinking = Some(level);
        self.publish_options(out);
    }

    /// Adds a session info item.
    pub(super) fn session_info(
        &mut self,
        at: At,
        title: Option<String>,
        model: Option<String>,
        thinking: Option<String>,
        out: &mut FoldOut,
    ) {
        self.items.create(
            &mut out.ops,
            item_id("info", at.seq, None),
            at,
            at.seq,
            ItemBody::SessionInfo {
                title,
                model,
                thinking,
            },
            Life::Done,
        );
    }

    /// A supervisor started a new pi process. Whatever the previous one left open ends (a
    /// killed supervisor leaves no `exit` record; the runtime notices that itself, so no
    /// `Exited` here), and the new process reports its native session again.
    pub(super) fn on_meta(&mut self, at: At, out: &mut FoldOut) {
        if self.process.alive {
            self.end_process(at, "pi stopped without an exit record", out);
        }
        self.view.native = None;
        self.process.alive = true;
    }

    /// pi exited: everything it had open ends, and the run (if any) fails.
    pub(super) fn on_exit(&mut self, at: At, exit: &ExitInfo, out: &mut FoldOut) {
        let reason = match (exit.code, exit.signal) {
            (_, Some(signal)) => format!("pi was killed by signal {signal}"),
            (Some(code), None) => format!("pi exited with code {code}"),
            (None, None) => "pi stopped".to_owned(),
        };
        let during_run = self.end_process(at, &reason, out);
        out.events.push(DriverEvent::Exited {
            code: exit.code,
            signal: exit.signal,
            during_run,
        });
    }

    /// A run is about to start (see [`crate::AgentView::run_starting`]): pi said `started`
    /// and `agent_start` hasn't come yet, or a run-starting `prompt` awaits its response.
    pub(super) fn run_starting(&self) -> bool {
        self.process.run.is_none()
            && (self.process.cause.is_some()
                || self.process.sent.values().any(|sent| {
                    matches!(
                        sent.command,
                        SentCommand::User {
                            starts_run: true,
                            ..
                        }
                    )
                }))
    }

    /// Ends everything the process had open and resets its state. Returns whether a run
    /// was in flight or about to start.
    fn end_process(&mut self, at: At, reason: &str, out: &mut FoldOut) -> bool {
        let starting = self.run_starting();
        self.end_message(at, out);
        self.close_tools(at, out);
        self.close_dialogs(at, RequestState::Withdrawn, |_| true, out);
        self.close_retry(at, None, out);
        self.close_compaction(at, None, None, Some(reason.to_owned()), out);
        let sent = std::mem::take(&mut self.process.sent);
        for sent in sent.into_values() {
            if let SentCommand::Bash { item } = sent.command {
                self.items.finish(&mut out.ops, &item, at, |body| {
                    if let ItemBody::ToolCall { status, .. } = body {
                        *status = ToolStatus::Cancelled;
                    }
                });
            }
        }
        let during_run = match self.process.run.take() {
            Some(run) => {
                let error = format!("{reason} during the run");
                self.finish_run(at, &run.item, RunOutcome::Failed, Some(error), out);
                true
            }
            None => starting,
        };
        self.process = Process::default();
        self.view.ready = false;
        self.view.run_active = false;
        self.view.open_tools = 0;
        self.view.open_requests.clear();
        self.view.unanswered.clear();
        during_run
    }
}

/// A settled run's outcome and error.
fn outcome(aborted: bool, run: &Run) -> (RunOutcome, Option<String>) {
    if aborted {
        return (RunOutcome::Cancelled, None);
    }
    let Some((reason, message)) = &run.last_stop else {
        return (RunOutcome::Completed, None);
    };
    match reason.as_str() {
        "stop" | "toolUse" => (RunOutcome::Completed, None),
        "length" => (RunOutcome::MaxTokens, None),
        "aborted" => (RunOutcome::Cancelled, None),
        "error" => (
            RunOutcome::Failed,
            run.retry_error.clone().or_else(|| message.clone()),
        ),
        _ => (RunOutcome::Unknown, message.clone()),
    }
}
