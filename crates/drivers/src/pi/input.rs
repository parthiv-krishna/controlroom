//! Lines written to pi's stdin and pi's `response`s to them.

use controlroom_protocol::session::{ContentPart, ContextUsage};
use controlroom_protocol::timeline::{
    ItemBody, MessageRole, RunTrigger, TokenUsage, ToolKind, ToolOutput, ToolStatus, UsageScope,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::items::{At, Life, item_id};
use super::options::model_label;
use super::state::{Cause, Echo, Sent, SentCommand};
use super::wire::{BashResult, InLine, Model, Response, SessionStats, State};
use super::{PiDriver, content, tag, tools};
use crate::{DriverEvent, FoldOut, LineTag, NativeRef};

impl PiDriver {
    /// Folds one line written to pi (its `Written` event is already out).
    pub(super) fn on_in(&mut self, at: At, line: InLine, out: &mut FoldOut) {
        if line.kind == "extension_ui_response" {
            let tag = line_tag(&line);
            self.on_answer(at, &line, tag, out);
            return;
        }
        let Some(id) = line.id.clone() else {
            return;
        };
        if let Some(tag) = tag::decode(&id) {
            self.view.unanswered.retain(|t| *t != tag);
            self.view.unanswered.push(tag);
        }
        let command = match line.kind.as_str() {
            "prompt" | "steer" | "follow_up" => SentCommand::User {
                content: content::from_line(line.message.as_deref().unwrap_or(""), &line.images),
            },
            "abort" => {
                self.process.aborts += 1;
                SentCommand::Abort
            }
            "bash" => {
                let command = line.command.unwrap_or_default();
                let item = item_id("bash", at.seq, None);
                self.items.create(
                    &mut out.ops,
                    item.clone(),
                    at,
                    at.seq,
                    ItemBody::ToolCall {
                        tool_call_id: id.clone(),
                        parent_tool_call_id: None,
                        name: "bash".into(),
                        title: tools::title("bash", &serde_json::json!({ "command": command })),
                        tool_kind: ToolKind::Execute,
                        input: Some(command),
                        status: ToolStatus::Running,
                        output: None,
                        exit: None,
                        duration_ms: None,
                    },
                    Life::Streaming,
                );
                SentCommand::Bash { item }
            }
            _ => SentCommand::Other,
        };
        self.process.sent.insert(id, Sent { at, command });
    }

    /// Folds a `response`. The first one shows pi is up and answering.
    pub(super) fn on_response(&mut self, at: At, response: Response, out: &mut FoldOut) {
        if !self.view.ready {
            self.view.ready = true;
            out.events.push(DriverEvent::Ready);
        }
        let tag = response.id.as_deref().and_then(tag::decode);
        let sent = response
            .id
            .as_deref()
            .and_then(|id| self.process.sent.remove(id));
        // A line that starts a run counts as accepted once the run is active (see `Cause`).
        let starts_run = response.success
            && !self.view.run_active
            && response.data.get("disposition").and_then(Value::as_str) == Some("started");
        if let Some(tag) = &tag
            && !starts_run
        {
            self.view.unanswered.retain(|t| t != tag);
        }
        if matches!(
            sent,
            Some(Sent {
                command: SentCommand::Abort,
                ..
            })
        ) {
            self.process.aborts = self.process.aborts.saturating_sub(1);
        }
        if !response.success {
            let error = response.error.unwrap_or_else(|| "unknown error".into());
            self.on_failure(at, &response.command, sent, &error, out);
            if let Some(tag) = tag {
                out.events.push(DriverEvent::Rejected { tag, error });
            }
            return;
        }
        if let Some(tag) = &tag
            && !starts_run
        {
            out.events.push(DriverEvent::Accepted { tag: tag.clone() });
        }
        let data = response.data;
        match (response.command.as_str(), sent) {
            ("prompt" | "steer" | "follow_up", sent) => {
                let disposition = data.get("disposition").and_then(Value::as_str);
                self.on_user_accepted(at, sent, disposition, tag, out);
            }
            (
                "bash",
                Some(Sent {
                    command: SentCommand::Bash { item },
                    ..
                }),
            ) => {
                let result: BashResult = decode(data);
                let status = if result.cancelled {
                    ToolStatus::Cancelled
                } else if result.exit_code.is_some_and(|code| code != 0) {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Completed
                };
                self.items.finish(&mut out.ops, &item, at, |body| {
                    if let ItemBody::ToolCall {
                        status: s,
                        output,
                        exit,
                        ..
                    } = body
                    {
                        *s = status;
                        *exit = result.exit_code;
                        if let Some(text) = result.output {
                            *output = Some(ToolOutput::Terminal { text });
                        }
                    }
                });
            }
            ("get_state", _) => self.on_state(decode(data), out),
            ("get_available_models", _) => {
                self.options.models = decode(data.get("models").cloned().unwrap_or_default());
                self.publish_options(out);
            }
            ("get_available_thinking_levels", _) => {
                self.options.levels = decode(data.get("levels").cloned().unwrap_or_default());
                self.publish_options(out);
            }
            ("set_model", _) => {
                let model: Model = decode(data);
                if !model.id.is_empty() {
                    self.session_info(at, None, Some(model_label(&model)), None, out);
                    self.options.model = Some(model);
                    self.publish_options(out);
                }
            }
            ("new_session", _) if data.get("cancelled") != Some(&Value::Bool(true)) => {
                // pi now writes to another session; `get_state` (Refresh) reports it.
                self.view.native = None;
            }
            ("get_session_stats", _) => self.on_stats(at, decode(data), out),
            _ => {}
        }
    }

    /// A command failed: its bash item fails, and the error shows in the timeline (a failed
    /// `compact` already shows in its compaction item).
    fn on_failure(
        &mut self,
        at: At,
        command: &str,
        sent: Option<Sent>,
        error: &str,
        out: &mut FoldOut,
    ) {
        if let Some(Sent {
            command: SentCommand::Bash { item },
            ..
        }) = sent
        {
            self.items.finish(&mut out.ops, &item, at, |body| {
                if let ItemBody::ToolCall { status, output, .. } = body {
                    *status = ToolStatus::Failed;
                    *output = Some(ToolOutput::Text { text: error.into() });
                }
            });
            return;
        }
        if command == "compact" {
            return;
        }
        let text = format!("pi rejected {command}: {error}");
        self.items.create(
            &mut out.ops,
            item_id("error", at.seq, None),
            at,
            at.seq,
            ItemBody::Error { text },
            Life::Done,
        );
    }

    /// pi accepted a prompt, steer or follow-up: show the user message, expect pi's echo
    /// unless an extension command handled it, and remember what starts the next run.
    fn on_user_accepted(
        &mut self,
        at: At,
        sent: Option<Sent>,
        disposition: Option<&str>,
        tag: Option<LineTag>,
        out: &mut FoldOut,
    ) {
        let started = disposition == Some("started");
        let written = sent.is_some();
        if let Some(Sent {
            at: from,
            command: SentCommand::User { content },
        }) = sent
        {
            let item = item_id("user", from.seq, None);
            if matches!(disposition, Some("started" | "queued")) {
                self.process.echoes.push(Echo {
                    item: item.clone(),
                    ts: from.ts,
                    text: content::text_of(&content),
                    content: content.clone(),
                    started,
                });
            }
            self.user_message(item, from, at, content, out);
        }
        if started {
            let trigger = match &tag {
                Some(LineTag::Op { .. }) => RunTrigger::User { device: None },
                Some(LineTag::Queue { .. }) => RunTrigger::Queue { device: None },
                Some(LineTag::Internal { .. }) => RunTrigger::Unknown,
                None if written => RunTrigger::User { device: None },
                None => RunTrigger::Unknown,
            };
            self.process.cause = Some(Cause { tag, trigger });
        }
    }

    /// Adds a finished user message.
    pub(super) fn user_message(
        &mut self,
        id: String,
        from: At,
        to: At,
        content: Vec<ContentPart>,
        out: &mut FoldOut,
    ) {
        self.items.create(
            &mut out.ops,
            id,
            from,
            to.seq,
            ItemBody::Message {
                role: MessageRole::User,
                content,
            },
            Life::Done,
        );
    }

    /// Folds `get_state`: options, the native session and its title.
    fn on_state(&mut self, state: State, out: &mut FoldOut) {
        if state.model.is_some() {
            self.options.model = state.model;
        }
        if state.thinking_level.is_some() {
            self.options.thinking = state.thinking_level;
        }
        self.publish_options(out);
        if let Some(id) = state.session_id {
            let native = NativeRef {
                id,
                path: state.session_file,
            };
            if self.view.native.as_ref() != Some(&native) {
                self.view.native = Some(native.clone());
                out.events.push(DriverEvent::NativeSession(native));
            }
        }
        if let Some(name) = state.session_name
            && self.title.as_ref() != Some(&name)
        {
            self.title = Some(name.clone());
            out.events.push(DriverEvent::Title(name));
        }
    }

    /// Folds `get_session_stats` into a session usage item and the context.
    fn on_stats(&mut self, at: At, stats: SessionStats, out: &mut FoldOut) {
        let context = stats.context_usage.and_then(|usage| {
            Some(ContextUsage {
                used: usage.tokens?,
                size: usage.context_window,
                cost: Some(self.cost),
            })
        });
        let tokens = TokenUsage {
            input: stats.tokens.input,
            output: stats.tokens.output,
            cache_read: stats.tokens.cache_read,
            cache_write: stats.tokens.cache_write,
        };
        self.items.create(
            &mut out.ops,
            item_id("usage", at.seq, None),
            at,
            at.seq,
            ItemBody::Usage {
                scope: UsageScope::Session,
                tokens,
                cost: stats.cost,
                context,
            },
            Life::Done,
        );
        if let Some(context) = context {
            out.events.push(DriverEvent::Context(context));
        }
    }

    /// Streams RPC `bash` output into its tool item.
    pub(super) fn on_bash_update(&mut self, at: At, id: &str, delta: &str, out: &mut FoldOut) {
        let Some(Sent {
            command: SentCommand::Bash { item },
            ..
        }) = self.process.sent.get(id)
        else {
            return;
        };
        let item = item.clone();
        if self
            .items
            .body(&item)
            .and_then(super::items::text_of)
            .is_none()
        {
            self.items.update(&mut out.ops, &item, at, |body| {
                if let ItemBody::ToolCall { output, .. } = body {
                    *output = Some(ToolOutput::Terminal {
                        text: String::new(),
                    });
                }
            });
        }
        self.items.append(&mut out.ops, &item, at, delta);
    }
}

/// The tag of a line written to pi: in `crTag` for dialog answers (whose `id` is pi's),
/// else in `id`.
pub(super) fn line_tag(line: &InLine) -> Option<LineTag> {
    let field = if line.kind == "extension_ui_response" {
        &line.cr_tag
    } else {
        &line.id
    };
    field.as_deref().and_then(tag::decode)
}

/// Decodes response data leniently: malformed data reads as the default.
fn decode<T: DeserializeOwned + Default>(data: Value) -> T {
    serde_json::from_value(data).unwrap_or_default()
}
