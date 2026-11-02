//! `PiDriver`: pi 1.2.0's native RPC mode (`pi --mode rpc`), spec §6.3.
//!
//! The fold follows the recordings in `fixtures/pi-rpc/1.2.0` (see that README's observations,
//! which win over the spec where they disagree):
//!
//! - a run spans `agent_start` to `agent_settled`; retries are inner `agent_start`/`agent_end`
//!   pairs, and the outcome comes from `agent_settled.aborted` first, then the last assistant
//!   `stopReason`;
//! - user messages come from the `prompt`/`steer`/`follow_up` lines once pi accepts them; pi's
//!   own echo of the line is matched and skipped, and its `system` message is never shown;
//! - dialogs (`extension_ui_request` select/confirm/input/editor) are blocking `dialog`
//!   requests; pi never reports a dialog timeout, so the fold expires them by record time;
//! - `Cancel` answers every open dialog before aborting, since pi's `abort` waits for them.
//!
//! Item and request ids are `<kind>-<seq>[.<index>]` of the record that started them, so a
//! re-fold yields the same ids. Every line written carries its [`LineTag`] in pi's `id` field
//! (`extension_ui_response`, whose `id` is the dialog's, carries it in `crTag`); see [`tag`].

mod content;
mod dialogs;
mod encode;
mod input;
mod items;
mod options;
mod run;
mod state;
mod stream;
mod tag;
mod tools;
mod ui;
mod wire;

use controlroom_protocol::session::DriverKind;
use controlroom_protocol::timeline::{ItemBody, NoticeLevel};
use controlroom_protocol::{Seq, UnixMillis};
use controlroom_transcript::{Body, Line, Record};

use self::items::{At, Items, Life, item_id};
use self::options::Options;
use self::state::Process;
use self::wire::Out;
use crate::{
    AgentView, Capabilities, Driver, DriverEvent, EncodeError, FoldOut, Intent, LineTag, StartMode,
};

/// The pi RPC driver for one session (see the module docs).
#[derive(Debug, Clone)]
pub struct PiDriver {
    view: AgentView,
    items: Items,
    process: Process,
    options: Options,
    /// The session title pi reported last.
    title: Option<String>,
    /// The cost of every assistant message folded so far, in USD.
    cost: f64,
}

impl Default for PiDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl PiDriver {
    /// A driver that has folded nothing yet.
    pub fn new() -> Self {
        Self {
            view: AgentView {
                capabilities: Capabilities {
                    steer: true,
                    follow_up: true,
                    resume: true,
                    new_session: true,
                },
                ..AgentView::default()
            },
            items: Items::default(),
            process: Process::default(),
            options: Options::default(),
            title: None,
            cost: 0.0,
        }
    }

    /// Folds one line pi wrote to stdout.
    fn on_out(&mut self, at: At, raw: &str, out: &mut FoldOut) {
        let Ok(event) = serde_json::from_str::<Out>(raw) else {
            return;
        };
        if !self.view.ready {
            self.view.ready = true;
            out.events.push(DriverEvent::Ready);
        }
        match event {
            Out::Response(response) => self.on_response(at, response, out),
            Out::AgentStart {} => self.on_agent_start(at, out),
            Out::AgentSettled { aborted } => self.on_settled(at, aborted, out),
            Out::MessageStart { message } => self.on_message_start(at, message, out),
            Out::MessageUpdate {
                assistant_message_event,
            } => self.on_assistant_event(at, assistant_message_event, out),
            Out::MessageEnd { message } => self.on_message_end(at, message, out),
            Out::ToolExecutionStart(event) => self.on_tool_start(at, event, out),
            Out::ToolExecutionUpdate(event) => self.on_tool_update(at, event, out),
            Out::ToolExecutionEnd(event) => self.on_tool_end(at, event, out),
            Out::CompactionStart { reason } => self.on_compaction_start(at, &reason, out),
            Out::CompactionEnd {
                result,
                aborted,
                error_message,
            } => self.on_compaction_end(at, result, aborted, error_message, out),
            Out::AutoRetryStart {
                attempt,
                max_attempts,
                delay_ms,
                error_message,
            } => self.on_retry_start(at, attempt, max_attempts, delay_ms, error_message, out),
            Out::AutoRetryEnd {
                success,
                attempt,
                final_error,
            } => self.on_retry_end(at, success, attempt, final_error, out),
            Out::SessionInfoChanged { name } => self.on_session_name(at, name, out),
            Out::ThinkingLevelChanged { level } => self.on_thinking_level(at, level, out),
            Out::ExtensionUiRequest(request) => self.on_ui_request(at, request, out),
            Out::ExtensionError {
                extension_path,
                event,
                error,
            } => {
                let text = format!(
                    "Extension error in {} ({}): {}",
                    extension_path.as_deref().unwrap_or("an extension"),
                    event.as_deref().unwrap_or("unknown event"),
                    error.as_deref().unwrap_or("unknown error"),
                );
                self.items.create(
                    &mut out.ops,
                    item_id("error", at.seq, None),
                    at,
                    at.seq,
                    ItemBody::Error { text },
                    Life::Done,
                );
            }
            Out::BashExecutionUpdate { id, delta } => self.on_bash_update(at, &id, &delta, out),
            Out::Unknown => {}
        }
    }

    /// Adds a finished notice item.
    fn notice(&mut self, at: At, level: NoticeLevel, text: String, out: &mut FoldOut) {
        self.items.create(
            &mut out.ops,
            item_id("notice", at.seq, None),
            at,
            at.seq,
            ItemBody::Notice { level, text },
            Life::Done,
        );
    }

    /// Reports the options if they changed.
    fn publish_options(&mut self, out: &mut FoldOut) {
        let options = self.options.neutral();
        if options != self.view.options {
            self.view.options = options.clone();
            out.events.push(DriverEvent::Options(options));
        }
    }
}

/// Whether a stdio line is complete (cut lines are never valid JSON worth reading).
fn complete(line: &Line) -> bool {
    line.truncated == 0
}

impl Driver for PiDriver {
    fn kind(&self) -> DriverKind {
        DriverKind::Pi
    }

    fn fold(&mut self, record: &Record) -> FoldOut {
        let at = At {
            seq: Seq(record.seq),
            ts: UnixMillis(i64::try_from(record.ts).unwrap_or(i64::MAX)),
        };
        let mut out = FoldOut::default();
        self.expire_dialogs(at, &mut out);
        match &record.body {
            Body::Meta(_) => self.on_meta(at, &mut out),
            Body::In(line) if complete(line) => {
                self.process.alive = true;
                self.on_in(at, &line.raw, &mut out);
            }
            Body::Out(line) if complete(line) => {
                self.process.alive = true;
                self.on_out(at, &line.raw, &mut out);
            }
            Body::Exit(exit) => self.on_exit(at, Some(exit), &mut out),
            Body::In(_) | Body::Out(_) | Body::Stderr(_) | Body::Unknown(_) => {}
        }
        self.view.last_seq = Some(at.seq);
        self.view.last_ts = Some(at.ts);
        out
    }

    fn view(&self) -> &AgentView {
        &self.view
    }

    fn encode(&mut self, intent: &Intent, tag: &LineTag) -> Result<Vec<String>, EncodeError> {
        self.encode_intent(intent, tag)
    }

    fn launch_args(&self, mode: &StartMode) -> Vec<String> {
        let mut args = vec!["--mode".to_owned(), "rpc".to_owned()];
        match mode {
            StartMode::New { native_id } => {
                args.extend(["--session-id".to_owned(), native_id.clone()]);
            }
            StartMode::Resume { native } => match &native.path {
                Some(path) => args.extend(["--session".to_owned(), path.clone()]),
                // `--session-id` opens the exact id in this project (never prompts).
                None => args.extend(["--session-id".to_owned(), native.id.clone()]),
            },
        }
        args
    }
}
