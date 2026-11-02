//! The materialized timeline: folded items stored by (session, item), so `session.subscribe
//! {since_seq}` is served from SQLite instead of re-folding the transcript.
//!
//! Ops change items in a per-session [`TimelineCache`]; a checkpoint writes the changed items
//! and moves the session's `timeline_seq`. After a restart the re-fold applies ops only for
//! records after `timeline_seq`, so text is never appended twice.

use std::collections::HashMap;

use controlroom_protocol::Seq;
use controlroom_protocol::ids::{ItemId, SessionId};
use controlroom_protocol::session::ContentPart;
use controlroom_protocol::timeline::{ItemBody, TimelineItem, TimelineOp, ToolOutput};

use crate::store::Store;

/// Checkpoint once this many items are dirty.
const MAX_DIRTY: usize = 64;

/// Items changed since the last checkpoint.
#[derive(Debug, Default)]
pub(crate) struct TimelineCache {
    /// Item → (item, seq of the record that last changed it).
    dirty: HashMap<ItemId, (TimelineItem, Seq)>,
}

impl TimelineCache {
    /// Applies an op from the record at `seq`. Items not in the cache are loaded from the store.
    pub(crate) fn apply(
        &mut self,
        store: &Store,
        session: SessionId,
        op: &TimelineOp,
        seq: Seq,
    ) -> rusqlite::Result<()> {
        match op {
            TimelineOp::Upsert { item } => {
                self.dirty.insert(item.id.clone(), ((**item).clone(), seq));
            }
            TimelineOp::AppendText {
                id, text, seq_to, ..
            } => {
                if let Some(item) = self.load(store, session, id)? {
                    append_text(&mut item.0, text);
                    item.0.seq_to = item.0.seq_to.max(*seq_to);
                    item.1 = seq;
                }
            }
            TimelineOp::End { id, seq_to, .. } => {
                if let Some(item) = self.load(store, session, id)? {
                    item.0.streaming = false;
                    item.0.seq_to = item.0.seq_to.max(*seq_to);
                    item.1 = seq;
                }
            }
            TimelineOp::Unknown => {}
        }
        Ok(())
    }

    fn load(
        &mut self,
        store: &Store,
        session: SessionId,
        id: &ItemId,
    ) -> rusqlite::Result<Option<&mut (TimelineItem, Seq)>> {
        if !self.dirty.contains_key(id) {
            let Some(stored) = store.timeline_item(session, id)? else {
                return Ok(None);
            };
            self.dirty.insert(id.clone(), stored);
        }
        Ok(self.dirty.get_mut(id))
    }

    /// Whether a checkpoint is due regardless of time.
    pub(crate) fn full(&self) -> bool {
        self.dirty.len() >= MAX_DIRTY
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.dirty.is_empty()
    }

    /// Writes every changed item (inside the caller's transaction) and empties the cache.
    pub(crate) fn flush(&mut self, store: &Store, session: SessionId) -> rusqlite::Result<()> {
        for (item, seq) in self.dirty.values() {
            store.put_timeline_item(session, item, *seq)?;
        }
        self.dirty.clear();
        Ok(())
    }
}

/// Appends streamed text where [`TimelineOp::AppendText`] says it goes.
fn append_text(item: &mut TimelineItem, text: &str) {
    match &mut item.body {
        ItemBody::Message { content, .. } => match content.last_mut() {
            Some(ContentPart::Text { text: last }) => last.push_str(text),
            _ => content.push(ContentPart::text(text)),
        },
        ItemBody::Thinking { text: buf } => buf.push_str(text),
        ItemBody::ToolCall { output, .. } => match output {
            Some(ToolOutput::Text { text: buf } | ToolOutput::Terminal { text: buf }) => {
                buf.push_str(text);
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

#[cfg(test)]
mod tests {
    use controlroom_protocol::UnixMillis;
    use controlroom_protocol::timeline::{Actor, MessageRole};

    use super::*;

    #[test]
    fn appends_to_the_last_text_part() {
        let mut item = TimelineItem {
            id: ItemId::new("m1"),
            seq_from: Seq(1),
            seq_to: Seq(1),
            actor: Actor::Root,
            ts: UnixMillis(0),
            streaming: true,
            body: ItemBody::Message {
                role: MessageRole::Agent,
                content: vec![ContentPart::text("he")],
            },
        };
        append_text(&mut item, "llo");
        assert_eq!(
            item.body,
            ItemBody::Message {
                role: MessageRole::Agent,
                content: vec![ContentPart::text("hello")]
            }
        );
    }
}
