//! Who sent each user-role line and why. A driver only knows the kind of a line's tag (a user
//! command, a queue item, a daemon-internal line); the runtime knows the device, the schedule,
//! the injection. Every user-role line the runtime writes records its [`MessageOrigin`] first
//! ([`SessionData::record_line`]): a queued prompt's device or schedule, a steered send-now's
//! device, an injection's origin and source, auto-continue. The fold's ops are then rewritten
//! with what was recorded, in this and every later op of the item:
//!
//! - the line's user message ([`DriverEvent::UserMessage`]) gets the origin;
//! - the run it starts ([`DriverEvent::RunStarted`], a steer counts: it starts a run if the
//!   active one ends first) gets the matching [`RunTrigger`].
//!
//! Messages without a recorded line (an extension's, or lines from before origins existed)
//! keep `origin: null`.

use controlroom_drivers::{DriverEvent, FoldOut};
use controlroom_protocol::session::PromptOrigin;
use controlroom_protocol::timeline::{ItemBody, MessageOrigin, RunTrigger, TimelineOp};

use crate::model::{QueueEntry, SessionData};
use crate::session::Session;

/// Puts the recorded triggers on the run items and origins on the user messages of a fold.
pub(crate) fn enrich(session: &mut Session, out: &mut FoldOut) {
    for event in &out.events {
        match event {
            DriverEvent::RunStarted { cause: Some(tag) } => {
                let Some(trigger) = session.data.trigger_for(tag).cloned() else {
                    continue;
                };
                for op in &out.ops {
                    if let TimelineOp::Upsert { item } = op
                        && matches!(item.body, ItemBody::Run { .. })
                    {
                        session
                            .derived
                            .run_triggers
                            .insert(item.id.clone(), trigger.clone());
                    }
                }
            }
            DriverEvent::UserMessage { item, tag } => {
                if let Some(origin) = session.data.origin_for(tag).cloned() {
                    session.derived.message_origins.insert(item.clone(), origin);
                }
            }
            _ => {}
        }
    }
    let derived = &session.derived;
    for op in &mut out.ops {
        let TimelineOp::Upsert { item } = op else {
            continue;
        };
        match &mut item.body {
            ItemBody::Run { trigger, .. } => {
                if let Some(known) = derived.run_triggers.get(&item.id) {
                    trigger.clone_from(known);
                }
            }
            ItemBody::Message { origin, .. } => {
                if let Some(known) = derived.message_origins.get(&item.id) {
                    *origin = Some(known.clone());
                }
            }
            _ => {}
        }
    }
}

/// The trigger of the run a line with this origin starts.
pub(crate) fn run_trigger(origin: &MessageOrigin) -> RunTrigger {
    match origin {
        MessageOrigin::User { device } => RunTrigger::User {
            device: device.clone(),
        },
        MessageOrigin::Queue { device } => RunTrigger::Queue {
            device: device.clone(),
        },
        MessageOrigin::Steer { device } => RunTrigger::Steer {
            device: device.clone(),
        },
        MessageOrigin::Schedule { schedule_id, name } => RunTrigger::Schedule {
            schedule_id: *schedule_id,
            name: name.clone(),
        },
        MessageOrigin::Inject { origin, label, .. } => RunTrigger::Inject {
            origin: *origin,
            label: label.clone(),
        },
        MessageOrigin::AutoContinue => RunTrigger::AutoContinue,
        MessageOrigin::Unknown => RunTrigger::Unknown,
    }
}

/// The origin of a queue item sent as a prompt: its schedule, else its sender's device (a
/// direct prompt to an idle agent is the user's, the others come from the queue).
fn of_queued_prompt(entry: &QueueEntry) -> MessageOrigin {
    match &entry.origin {
        PromptOrigin::Schedule { schedule_id, name } => MessageOrigin::Schedule {
            schedule_id: *schedule_id,
            name: name.clone(),
        },
        PromptOrigin::User { device } if entry.direct => MessageOrigin::User {
            device: device.clone(),
        },
        PromptOrigin::User { device } => MessageOrigin::Queue {
            device: device.clone(),
        },
        PromptOrigin::Unknown if entry.direct => MessageOrigin::User { device: None },
        PromptOrigin::Unknown => MessageOrigin::Queue { device: None },
    }
}

/// Records the origin of the queue item about to be sent as a prompt.
pub(crate) fn record_queue_entry(data: &mut SessionData, entry: &QueueEntry) {
    data.record_line(&entry.tag(), of_queued_prompt(entry));
}

/// Records the origin of the send-now item about to be steered in: its schedule, else its
/// sender's device. A steer starts the next run when the active one settles before the agent
/// reads it.
pub(crate) fn record_steered_entry(data: &mut SessionData, entry: &QueueEntry) {
    let origin = match &entry.origin {
        PromptOrigin::Schedule { schedule_id, name } => MessageOrigin::Schedule {
            schedule_id: *schedule_id,
            name: name.clone(),
        },
        PromptOrigin::User { device } => MessageOrigin::Steer {
            device: device.clone(),
        },
        PromptOrigin::Unknown => MessageOrigin::Steer { device: None },
    };
    data.record_line(&entry.tag(), origin);
}
