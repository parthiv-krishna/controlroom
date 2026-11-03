//! Timeline items the daemon makes itself, outside the agent's transcript: raised requests,
//! todo snapshots and the agent's notices.
//!
//! **Placement.** Such an item has no transcript seq of its own. It sits at the transcript head
//! when it was made (`seq_from = seq_to = head`), and every change to it is stored as changed
//! *after* the current head, so `session.subscribe {since_seq: head}` (a reconnect, or a feed
//! catching up after it lagged) still delivers it (possibly again; upserts are idempotent).

use controlroom_protocol::RpcError;
use controlroom_protocol::ids::{ItemId, Seq, SessionId};
use controlroom_protocol::timeline::{Actor, ItemBody, TimelineItem, TimelineOp};

use crate::core::Core;
use crate::errors::internal;
use crate::session::SessionUpdate;

impl Core {
    /// Creates or replaces a daemon-made item. A new item sits at the head; an existing one
    /// keeps its place and time.
    pub(crate) fn put_item(
        &mut self,
        id: SessionId,
        item_id: ItemId,
        body: ItemBody,
    ) -> Result<(), RpcError> {
        let head = self.head(id);
        let existing = self.store.timeline_item(id, &item_id).map_err(internal)?;
        let (seq, ts) = match existing {
            Some((item, _)) => (item.seq_from, item.ts),
            None => (head, self.clock.now()),
        };
        self.put_item_at(id, item_id, body, seq, ts)
    }

    /// Writes a daemon-made item at `seq` and sends it to the session's subscribers.
    pub(crate) fn put_item_at(
        &mut self,
        id: SessionId,
        item_id: ItemId,
        body: ItemBody,
        seq: Seq,
        ts: controlroom_protocol::UnixMillis,
    ) -> Result<(), RpcError> {
        let head = self.head(id);
        let item = TimelineItem {
            id: item_id,
            seq_from: seq,
            seq_to: seq,
            actor: Actor::Root,
            ts,
            streaming: false,
            body,
        };
        // Changed "after" the head: see the module docs.
        self.store
            .put_timeline_item(id, &item, Seq(head.0 + 1))
            .map_err(internal)?;
        self.publish(
            id,
            SessionUpdate {
                head_seq: head,
                ops: vec![TimelineOp::Upsert {
                    item: Box::new(item),
                }],
                raw: None,
                detail: None,
            },
        );
        Ok(())
    }
}
