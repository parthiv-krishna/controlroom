//! The fold's live timeline items: those that may still change. Every change both emits an op
//! and updates the stored copy exactly as a materializer would, so a later whole-item upsert
//! carries everything streamed so far. Finished items are dropped to keep the state bounded.

use std::collections::BTreeMap;

use controlroom_protocol::ids::ItemId;
use controlroom_protocol::session::ContentPart;
use controlroom_protocol::timeline::{Actor, ItemBody, TimelineItem, TimelineOp, ToolOutput};
use controlroom_protocol::{Seq, UnixMillis};

/// Where a change comes from: the record being folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct At {
    pub seq: Seq,
    pub ts: UnixMillis,
}

/// An item id from a kind and the record (and sub-index) it started at, e.g. `run-5`, `msg-11.0`.
pub(crate) fn item_id(kind: &str, seq: Seq, sub: Option<&str>) -> String {
    match sub {
        Some(sub) => format!("{kind}-{}.{sub}", seq.0),
        None => format!("{kind}-{}", seq.0),
    }
}

/// Whether a new item may still change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Life {
    /// Final as created.
    Done,
    /// Kept for later updates (runs, requests, retries, compactions).
    Open,
    /// Kept, and marked `streaming` until it finishes.
    Streaming,
}

/// Live items by id.
#[derive(Debug, Clone, Default)]
pub(crate) struct Items {
    live: BTreeMap<String, TimelineItem>,
}

impl Items {
    /// Inserts an item that started at `from` and was last touched at `to`, and emits it.
    pub(crate) fn create(
        &mut self,
        ops: &mut Vec<TimelineOp>,
        id: String,
        from: At,
        to: Seq,
        body: ItemBody,
        life: Life,
    ) {
        let streaming = life == Life::Streaming;
        let item = TimelineItem {
            id: ItemId::new(id.as_str()),
            seq_from: from.seq,
            seq_to: to,
            actor: Actor::Root,
            ts: from.ts,
            streaming,
            body,
        };
        ops.push(upsert(&item));
        if life != Life::Done {
            self.live.insert(id, item);
        }
    }

    /// The body of a live item.
    pub(crate) fn body(&self, id: &str) -> Option<&ItemBody> {
        self.live.get(id).map(|item| &item.body)
    }

    /// Changes a live item's body and re-emits it whole. Returns whether the item was live.
    pub(crate) fn update(
        &mut self,
        ops: &mut Vec<TimelineOp>,
        id: &str,
        at: At,
        change: impl FnOnce(&mut ItemBody),
    ) -> bool {
        let Some(item) = self.live.get_mut(id) else {
            return false;
        };
        change(&mut item.body);
        item.seq_to = at.seq;
        ops.push(upsert(item));
        true
    }

    /// Like [`Items::update`], then marks the item finished and drops it.
    pub(crate) fn finish(
        &mut self,
        ops: &mut Vec<TimelineOp>,
        id: &str,
        at: At,
        change: impl FnOnce(&mut ItemBody),
    ) -> bool {
        let Some(mut item) = self.live.remove(id) else {
            return false;
        };
        change(&mut item.body);
        item.seq_to = at.seq;
        item.streaming = false;
        ops.push(upsert(&item));
        true
    }

    /// Appends streamed text (message, thinking or tool output).
    pub(crate) fn append(&mut self, ops: &mut Vec<TimelineOp>, id: &str, at: At, text: &str) {
        let Some(item) = self.live.get_mut(id) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        append_text(&mut item.body, text);
        item.seq_to = at.seq;
        ops.push(TimelineOp::AppendText {
            id: item.id.clone(),
            text: text.to_owned(),
            seq_from: at.seq,
            seq_to: at.seq,
        });
    }

    /// Ends a streaming item as it is and drops it.
    pub(crate) fn end(&mut self, ops: &mut Vec<TimelineOp>, id: &str, at: At) {
        if let Some(item) = self.live.remove(id) {
            ops.push(TimelineOp::End {
                id: item.id,
                seq_from: at.seq,
                seq_to: at.seq,
            });
        }
    }
}

fn upsert(item: &TimelineItem) -> TimelineOp {
    TimelineOp::Upsert {
        item: Box::new(item.clone()),
    }
}

/// Applies `append_text` to a body the way the protocol defines it: a message's last text
/// part (adding one if needed), a thinking item's text, or a tool call's text output.
pub(crate) fn append_text(body: &mut ItemBody, text: &str) {
    match body {
        ItemBody::Message { content, .. } => match content.last_mut() {
            Some(ContentPart::Text { text: last }) => last.push_str(text),
            _ => content.push(ContentPart::text(text)),
        },
        ItemBody::Thinking { text: thinking } => thinking.push_str(text),
        ItemBody::ToolCall { output, .. } => match output {
            Some(ToolOutput::Text { text: out } | ToolOutput::Terminal { text: out }) => {
                out.push_str(text);
            }
            _ => {
                *output = Some(ToolOutput::Text {
                    text: text.to_owned(),
                });
            }
        },
        _ => {}
    }
}

/// The text a message, thinking item or tool output holds.
pub(crate) fn text_of(body: &ItemBody) -> Option<&str> {
    match body {
        ItemBody::Message { content, .. } => match content.last() {
            Some(ContentPart::Text { text }) => Some(text),
            _ => Some(""),
        },
        ItemBody::Thinking { text } => Some(text),
        ItemBody::ToolCall {
            output: Some(ToolOutput::Text { text } | ToolOutput::Terminal { text }),
            ..
        } => Some(text),
        _ => None,
    }
}
