//! Messages (user echoes, streamed assistant blocks, usage) and tool calls.

use controlroom_protocol::session::{ContentPart, ContextUsage};
use controlroom_protocol::timeline::{
    ItemBody, MessageRole, TokenUsage, ToolOutput, ToolStatus, UsageScope,
};
use serde_json::Value;

use super::items::{At, Life, item_id, text_of};
use super::state::{Streamed, StreamedBlock, Tool};
use super::wire::{AssistantEvent, Block, Message, ToolCall, ToolEvent, Usage, parts_text};
use super::{PiDriver, content, tools};
use crate::{DriverEvent, FoldOut};

/// Whether a text block is agent text or reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextKind {
    Text,
    Thinking,
}

impl PiDriver {
    /// `message_start`: user and custom messages are complete here; assistant messages stream.
    pub(super) fn on_message_start(&mut self, at: At, message: Message, out: &mut FoldOut) {
        match message {
            Message::User { content } => {
                let parts = content::from_user(&content);
                self.on_user_echo(at, parts, out);
            }
            Message::Custom {
                content,
                display: true,
            } => {
                self.items.create(
                    &mut out.ops,
                    item_id("notice", at.seq, None),
                    at,
                    at.seq,
                    ItemBody::Message {
                        role: MessageRole::Notice,
                        content: content::from_user(&content),
                    },
                    Life::Done,
                );
            }
            Message::Assistant { .. } => {
                self.end_message(at, out);
                self.process.message = Some(Streamed {
                    seq: at.seq,
                    blocks: Default::default(),
                });
            }
            Message::Custom { .. } | Message::Other => {}
        }
    }

    /// pi delivered a user message. It is the echo of a line we showed when pi accepted it:
    /// the one with the same text, else the prompt that started the run (pi may have expanded
    /// it). A queued steer or follow-up moves to where pi delivered it; a message without a
    /// line (e.g. from an extension) is shown here.
    fn on_user_echo(&mut self, at: At, parts: Vec<ContentPart>, out: &mut FoldOut) {
        let echoes = &self.process.echoes;
        let text = content::text_of(&parts);
        let index = echoes
            .iter()
            .position(|echo| echo.text == text)
            .or_else(|| echoes.iter().position(|echo| echo.started));
        let Some(index) = index else {
            self.user_message(item_id("user", at.seq, None), at, at, parts, out);
            return;
        };
        let echo = self.process.echoes.remove(index);
        if !echo.started {
            let from = At {
                seq: at.seq,
                ts: echo.ts,
            };
            self.user_message(echo.item, from, at, echo.content, out);
        }
    }

    /// `message_update`: streamed assistant blocks.
    pub(super) fn on_assistant_event(&mut self, at: At, event: AssistantEvent, out: &mut FoldOut) {
        let index = event.content_index.unwrap_or(0);
        match event.kind.as_str() {
            "text_start" => {
                self.text_block(at, index, TextKind::Text, out);
            }
            "thinking_start" => {
                self.text_block(at, index, TextKind::Thinking, out);
            }
            "text_delta" | "thinking_delta" => {
                let kind = text_kind(&event.kind);
                if let (Some(item), Some(delta)) =
                    (self.text_block(at, index, kind, out), event.delta)
                {
                    self.items.append(&mut out.ops, &item, at, &delta);
                }
            }
            "text_end" | "thinking_end" => {
                let kind = text_kind(&event.kind);
                if let Some(item) = self.text_block(at, index, kind, out) {
                    self.finish_text(at, &item, event.content.as_deref(), out);
                }
            }
            "toolcall_start" => {
                if let Some(id) = event.id {
                    let call = ToolCall {
                        id,
                        name: event.tool_name.unwrap_or_default(),
                        arguments: Value::Null,
                    };
                    self.tool_block(at, index, &call, out);
                }
            }
            "toolcall_end" => {
                if let Some(call) = event.tool_call {
                    self.tool_block(at, index, &call, out);
                }
            }
            _ => {}
        }
    }

    /// `message_end`: the authoritative assistant message. Fills in blocks that never
    /// streamed, finishes the rest, records the stop reason and reports usage.
    pub(super) fn on_message_end(&mut self, at: At, message: Message, out: &mut FoldOut) {
        let Message::Assistant {
            content,
            stop_reason,
            error_message,
            usage,
            provider,
            model,
        } = message
        else {
            return;
        };
        for (index, block) in content.iter().enumerate() {
            match block {
                Block::Text { text } => self.final_text(at, index, TextKind::Text, text, out),
                Block::Thinking { thinking } => {
                    self.final_text(at, index, TextKind::Thinking, thinking, out);
                }
                Block::ToolCall(call) => self.tool_block(at, index, call, out),
                Block::Unknown => {}
            }
        }
        let stop_reason = stop_reason.unwrap_or_default();
        if stop_reason != "toolUse" {
            // These calls will never run.
            self.cancel_message_tools(at, out);
        }
        self.end_message(at, out);
        if let Some(run) = &mut self.process.run {
            run.last_stop = Some((stop_reason, error_message));
        }
        if let Some(usage) = usage {
            self.turn_usage(at, &usage, provider.as_deref(), model.as_deref(), out);
        }
    }

    /// The live item of a text block, created on first sight.
    fn text_block(
        &mut self,
        at: At,
        index: usize,
        kind: TextKind,
        out: &mut FoldOut,
    ) -> Option<String> {
        let message = self.streamed(at);
        let seq = message.seq;
        match message.blocks.get(&index) {
            Some(StreamedBlock::Text { item }) => return Some(item.clone()),
            Some(StreamedBlock::Tool { .. }) => return None,
            None => {}
        }
        let item = item_id("msg", seq, Some(&index.to_string()));
        message
            .blocks
            .insert(index, StreamedBlock::Text { item: item.clone() });
        let body = match kind {
            TextKind::Text => ItemBody::Message {
                role: MessageRole::Agent,
                content: Vec::new(),
            },
            TextKind::Thinking => ItemBody::Thinking {
                text: String::new(),
            },
        };
        self.items.create(
            &mut out.ops,
            item.clone(),
            at,
            at.seq,
            body,
            Life::Streaming,
        );
        Some(item)
    }

    /// The final text of a block from `message_end`, for blocks that may not have streamed.
    fn final_text(&mut self, at: At, index: usize, kind: TextKind, text: &str, out: &mut FoldOut) {
        let known = self
            .process
            .message
            .as_ref()
            .is_some_and(|message| message.blocks.contains_key(&index));
        if !known && text.is_empty() {
            return;
        }
        if let Some(item) = self.text_block(at, index, kind, out) {
            self.finish_text(at, &item, Some(text), out);
        }
    }

    /// Ends a text block: a cheap `end` when the streamed text is complete, else a whole
    /// upsert with the final text.
    fn finish_text(&mut self, at: At, item: &str, last: Option<&str>, out: &mut FoldOut) {
        let streamed = self.items.body(item).and_then(text_of);
        match (last, streamed) {
            (Some(last), Some(streamed)) if last != streamed => {
                let last = last.to_owned();
                self.items
                    .finish(&mut out.ops, item, at, |body| match body {
                        ItemBody::Message { content, .. } => {
                            *content = vec![ContentPart::text(last)];
                        }
                        ItemBody::Thinking { text } => *text = last,
                        _ => {}
                    });
            }
            _ => self.items.end(&mut out.ops, item, at),
        }
    }

    /// Ends whatever is still streaming of the current assistant message.
    pub(super) fn end_message(&mut self, at: At, out: &mut FoldOut) {
        let Some(message) = self.process.message.take() else {
            return;
        };
        for block in message.blocks.into_values() {
            if let StreamedBlock::Text { item } = block {
                self.items.end(&mut out.ops, &item, at);
            }
        }
    }

    /// The assistant message being streamed, started here if its `message_start` is missing.
    fn streamed(&mut self, at: At) -> &mut Streamed {
        self.process.message.get_or_insert_with(|| Streamed {
            seq: at.seq,
            blocks: Default::default(),
        })
    }

    /// Registers a tool call block and creates or completes its item.
    fn tool_block(&mut self, at: At, index: usize, call: &ToolCall, out: &mut FoldOut) {
        let message = self.streamed(at);
        let seq = message.seq;
        message.blocks.insert(
            index,
            StreamedBlock::Tool {
                call_id: call.id.clone(),
            },
        );
        let has_args = !call.arguments.is_null();
        if let Some(tool) = self.process.tools.get_mut(&call.id) {
            if !has_args || tool.args == call.arguments || tool.running {
                return;
            }
            tool.args = call.arguments.clone();
            let (item, name, args) = (tool.item.clone(), tool.name.clone(), tool.args.clone());
            self.items.update(&mut out.ops, &item, at, |body| {
                if let ItemBody::ToolCall { title, input, .. } = body {
                    *title = tools::title(&name, &args);
                    *input = tools::input(&name, &args);
                }
            });
            return;
        }
        let item = item_id("msg", seq, Some(&index.to_string()));
        self.create_tool(at, item, call, None, ToolStatus::Pending, out);
    }

    fn create_tool(
        &mut self,
        at: At,
        item: String,
        call: &ToolCall,
        parent: Option<String>,
        status: ToolStatus,
        out: &mut FoldOut,
    ) {
        self.items.create(
            &mut out.ops,
            item.clone(),
            at,
            at.seq,
            ItemBody::ToolCall {
                tool_call_id: call.id.clone(),
                parent_tool_call_id: parent,
                name: call.name.clone(),
                title: tools::title(&call.name, &call.arguments),
                tool_kind: tools::kind(&call.name),
                input: tools::input(&call.name, &call.arguments),
                status,
                output: None,
                exit: None,
                duration_ms: None,
            },
            Life::Streaming,
        );
        self.process.tools.insert(
            call.id.clone(),
            Tool {
                item,
                name: call.name.clone(),
                args: call.arguments.clone(),
                running: status == ToolStatus::Running,
            },
        );
        self.count_tools();
    }

    /// Cancels the not-yet-running tool calls of the current assistant message.
    fn cancel_message_tools(&mut self, at: At, out: &mut FoldOut) {
        let Some(message) = &self.process.message else {
            return;
        };
        let calls: Vec<String> = message
            .blocks
            .values()
            .filter_map(|block| match block {
                StreamedBlock::Tool { call_id } => Some(call_id.clone()),
                StreamedBlock::Text { .. } => None,
            })
            .collect();
        for call in calls {
            if self
                .process
                .tools
                .get(&call)
                .is_some_and(|tool| !tool.running)
            {
                self.end_tool(at, &call, ToolStatus::Cancelled, out);
            }
        }
    }

    /// Ends every tracked tool call as cancelled (the run or the process ended).
    pub(super) fn close_tools(&mut self, at: At, out: &mut FoldOut) {
        let calls: Vec<String> = self.process.tools.keys().cloned().collect();
        for call in calls {
            self.end_tool(at, &call, ToolStatus::Cancelled, out);
        }
    }

    fn end_tool(&mut self, at: At, call: &str, status: ToolStatus, out: &mut FoldOut) {
        if let Some(tool) = self.process.tools.remove(call) {
            self.items.finish(&mut out.ops, &tool.item, at, |body| {
                if let ItemBody::ToolCall { status: s, .. } = body {
                    *s = status;
                }
            });
        }
        self.count_tools();
    }

    fn count_tools(&mut self) {
        self.view.open_tools = self.process.tools.values().filter(|t| t.running).count();
    }

    /// `tool_execution_start`.
    pub(super) fn on_tool_start(&mut self, at: At, event: ToolEvent, out: &mut FoldOut) {
        let call = ToolCall {
            id: event.tool_call_id,
            name: event.tool_name,
            arguments: event.args,
        };
        let Some(tool) = self.process.tools.get_mut(&call.id) else {
            let item = item_id("tool", at.seq, None);
            let parent = event.parent_tool_call_id;
            self.create_tool(at, item, &call, parent, ToolStatus::Running, out);
            return;
        };
        tool.running = true;
        tool.args = call.arguments.clone();
        let item = tool.item.clone();
        self.items.update(&mut out.ops, &item, at, |body| {
            if let ItemBody::ToolCall {
                parent_tool_call_id,
                title,
                input,
                status,
                ..
            } = body
            {
                *status = ToolStatus::Running;
                *title = tools::title(&call.name, &call.arguments);
                *input = tools::input(&call.name, &call.arguments);
                if event.parent_tool_call_id.is_some() {
                    *parent_tool_call_id = event.parent_tool_call_id;
                }
            }
        });
        self.count_tools();
    }

    /// `tool_execution_update`: pi sends the output so far; stream what is new.
    pub(super) fn on_tool_update(&mut self, at: At, event: ToolEvent, out: &mut FoldOut) {
        let Some(tool) = self.process.tools.get(&event.tool_call_id) else {
            return;
        };
        let (item, name) = (tool.item.clone(), tool.name.clone());
        let text = parts_text(&event.partial_result.unwrap_or_default().content);
        let shown = self.items.body(&item).and_then(text_of).map(str::to_owned);
        match shown {
            Some(shown) if text.starts_with(&shown) => {
                self.items
                    .append(&mut out.ops, &item, at, &text[shown.len()..]);
            }
            None if text.is_empty() => {}
            _ => {
                self.items.update(&mut out.ops, &item, at, |body| {
                    if let ItemBody::ToolCall { output, .. } = body {
                        *output = Some(tools::text_output(&name, text));
                    }
                });
            }
        }
    }

    /// `tool_execution_end`. An error without `durationMs` never ran (blocked by a hook or
    /// aborted before it started, obs. 12), and one while we abort was aborted: both count as
    /// cancelled.
    pub(super) fn on_tool_end(&mut self, at: At, event: ToolEvent, out: &mut FoldOut) {
        if !self.process.tools.contains_key(&event.tool_call_id) {
            let call = ToolCall {
                id: event.tool_call_id.clone(),
                name: event.tool_name.clone(),
                arguments: event.args.clone(),
            };
            let item = item_id("tool", at.seq, None);
            let parent = event.parent_tool_call_id.clone();
            self.create_tool(at, item, &call, parent, ToolStatus::Running, out);
        }
        let Some(tool) = self.process.tools.remove(&event.tool_call_id) else {
            return;
        };
        let status = match (event.is_error, event.duration_ms) {
            (false, _) => ToolStatus::Completed,
            (true, None) => ToolStatus::Cancelled,
            (true, Some(_)) if self.process.aborts > 0 => ToolStatus::Cancelled,
            (true, Some(_)) => ToolStatus::Failed,
        };
        let result = event.result.unwrap_or_default();
        let text = parts_text(&result.content);
        let output: ToolOutput = tools::final_output(&tool.name, &tool.args, event.is_error, text);
        let exit = tools::exit_code(&result.structured_content);
        self.items.finish(&mut out.ops, &tool.item, at, |body| {
            if let ItemBody::ToolCall {
                status: s,
                output: o,
                exit: e,
                duration_ms,
                ..
            } = body
            {
                *s = status;
                *o = Some(output);
                *e = exit;
                *duration_ms = event.duration_ms;
            }
        });
        self.count_tools();
    }

    /// Reports one assistant message's usage as a turn usage item and the new context.
    fn turn_usage(
        &mut self,
        at: At,
        usage: &Usage,
        provider: Option<&str>,
        model: Option<&str>,
        out: &mut FoldOut,
    ) {
        let total = if usage.total_tokens > 0 {
            usage.total_tokens
        } else {
            usage.input + usage.output + usage.cache_read + usage.cache_write
        };
        if total == 0 {
            return;
        }
        let cost = usage.cost.as_ref().map(|cost| cost.total);
        self.cost += cost.unwrap_or(0.0);
        let context = ContextUsage {
            used: total,
            size: self.options.context_window(provider, model),
            cost: Some(self.cost),
        };
        self.items.create(
            &mut out.ops,
            item_id("usage", at.seq, None),
            at,
            at.seq,
            ItemBody::Usage {
                scope: UsageScope::Turn,
                tokens: TokenUsage {
                    input: usage.input,
                    output: usage.output,
                    cache_read: usage.cache_read,
                    cache_write: usage.cache_write,
                },
                cost,
                context: Some(context),
            },
            Life::Done,
        );
        out.events.push(DriverEvent::Context(context));
    }
}

fn text_kind(event: &str) -> TextKind {
    if event.starts_with("thinking") {
        TextKind::Thinking
    } else {
        TextKind::Text
    }
}
