//! The agent's notices (`notify_user`, spec §8).
//!
//! An info notice is only a timeline item (the run it belongs to makes the session unread). A
//! warning or an error is *pinned*: until the user dismisses it with `notice.dismiss` it is
//! listed in the summary's `notices` and raises the `notice` attention, so the controller
//! pushes it and clients pin it above the composer. A dismissal is a user action recorded with
//! its device; the agent isn't told. Opening or reading the session dismisses nothing.
//!
//! The timeline item is the record (`pinned`, `dismissed`): the open notices are loaded back
//! from the stored timeline when a session is loaded (`store::rows`). A closed session's open
//! notices are kept but not listed until it is reopened.

use controlroom_protocol::agent::NotifyParams;
use controlroom_protocol::agentd::NoticeDismissParams;
use controlroom_protocol::ids::{ItemId, SessionId};
use controlroom_protocol::session::{OpenNotice, SessionState};
use controlroom_protocol::timeline::{ItemBody, NoticeDismissal, TimelineItem};
use controlroom_protocol::{Resource, RpcError};

use crate::core::Core;
use crate::errors::{internal, invalid_params, invalid_state, not_found};
use crate::runtime::Runtime;

/// Longest notice text.
const MAX_NOTICE: usize = 4000;

impl Runtime {
    /// `notice.dismiss`: unpins an open notice (a no-op for one already dismissed).
    pub async fn dismiss_notice(&self, params: NoticeDismissParams) -> Result<(), RpcError> {
        self.command("notice.dismiss", &params.op_id, &params, |core| {
            core.dismiss_notice(&params)
        })
        .await
    }
}

impl Core {
    /// The agent's `notify`: a notice in the timeline, pinned if it is a warning or an error.
    pub(crate) fn agent_notice(
        &mut self,
        id: SessionId,
        params: &NotifyParams,
    ) -> Result<(), RpcError> {
        let text = params.text.trim();
        if text.is_empty() || text.len() > MAX_NOTICE {
            return Err(invalid_params(format!(
                "a notice has 1 to {MAX_NOTICE} bytes"
            )));
        }
        let head = self.head(id);
        let now = self.clock.now();
        let session = self.session_mut(id)?;
        if session.data.closed() {
            return Err(invalid_state(SessionState::Closed, "notify"));
        }
        let item_id = ItemId::new(format!("notice-{}", ulid::Ulid::generate()));
        let pinned = params.level.pins();
        if pinned {
            session.data.notices.push(OpenNotice {
                item_id: item_id.clone(),
                level: params.level,
                text: text.to_owned(),
                created_at: now,
                origin_child: None,
            });
        }
        let body = ItemBody::Notice {
            level: params.level,
            text: text.to_owned(),
            pinned,
            dismissed: None,
        };
        self.put_item_at(id, item_id, body, head, now)
    }

    fn dismiss_notice(&mut self, params: &NoticeDismissParams) -> Result<(), RpcError> {
        let id = params.id;
        let now = self.clock.now();
        self.session(id)?;
        let stored = self
            .store
            .timeline_item(id, &params.item_id)
            .map_err(internal)?;
        let Some((
            TimelineItem {
                body:
                    ItemBody::Notice {
                        level,
                        text,
                        pinned: true,
                        dismissed,
                    },
                ..
            },
            _,
        )) = stored
        else {
            return Err(not_found(Resource::Notice, &params.item_id));
        };
        if dismissed.is_some() {
            // Dismissed already, e.g. from another device.
            return Ok(());
        }
        self.session_mut(id)?
            .data
            .notices
            .retain(|n| n.item_id != params.item_id);
        let body = ItemBody::Notice {
            level,
            text,
            pinned: true,
            dismissed: Some(NoticeDismissal {
                by: params.by.clone(),
                at: now,
            }),
        };
        self.put_item(id, params.item_id.clone(), body)
    }
}

/// The open notice a timeline item shows: a pinned notice not yet dismissed.
pub(crate) fn open(item: &TimelineItem) -> Option<OpenNotice> {
    match &item.body {
        ItemBody::Notice {
            level,
            text,
            pinned: true,
            dismissed: None,
        } => Some(OpenNotice {
            item_id: item.id.clone(),
            level: *level,
            text: text.clone(),
            created_at: item.ts,
            origin_child: None,
        }),
        _ => None,
    }
}
