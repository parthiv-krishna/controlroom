//! Run triggers. A driver only knows the kind of the tag that started a run (a user command, a
//! queue item, a daemon-internal line); the runtime knows who sent it and why. Each
//! run-starting line the runtime writes records its trigger ([`SessionData::record_trigger`]):
//! a queued prompt's device or schedule, a steered send-now's device, an injection's origin,
//! auto-continue (a steer counts: it starts a run if the active one ends first). When the fold
//! reports the run, the run item is rewritten with that trigger, in this and every later op
//! of the run.

use controlroom_drivers::{DriverEvent, FoldOut};
use controlroom_protocol::session::PromptOrigin;
use controlroom_protocol::timeline::{ItemBody, RunTrigger, TimelineOp};

use crate::model::{QueueEntry, SessionData};
use crate::session::Session;

/// Puts the recorded trigger on the run items of a fold.
pub(crate) fn enrich(session: &mut Session, out: &mut FoldOut) {
    for event in &out.events {
        let DriverEvent::RunStarted { cause: Some(tag) } = event else {
            continue;
        };
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
    for op in &mut out.ops {
        if let TimelineOp::Upsert { item } = op
            && let ItemBody::Run { trigger, .. } = &mut item.body
            && let Some(known) = session.derived.run_triggers.get(&item.id)
        {
            trigger.clone_from(known);
        }
    }
}

/// The trigger of a run a queue item starts: its schedule, else its sender's device.
pub(crate) fn of_queue_entry(entry: &QueueEntry) -> RunTrigger {
    match &entry.origin {
        PromptOrigin::Schedule { schedule_id, name } => RunTrigger::Schedule {
            schedule_id: *schedule_id,
            name: name.clone(),
        },
        PromptOrigin::User { device } if entry.direct => RunTrigger::User {
            device: device.clone(),
        },
        PromptOrigin::User { device } => RunTrigger::Queue {
            device: device.clone(),
        },
        PromptOrigin::Unknown if entry.direct => RunTrigger::User { device: None },
        PromptOrigin::Unknown => RunTrigger::Queue { device: None },
    }
}

/// Records the trigger of the queue item about to be sent as a prompt.
pub(crate) fn record_queue_entry(data: &mut SessionData, entry: &QueueEntry) {
    data.record_trigger(&entry.tag(), of_queue_entry(entry));
}

/// Records the trigger of the send-now item about to be steered in: a steer starts the next
/// run when the active one settles before the agent reads it.
pub(crate) fn record_steered_entry(data: &mut SessionData, entry: &QueueEntry) {
    let device = match &entry.origin {
        PromptOrigin::User { device } => device.clone(),
        PromptOrigin::Schedule { .. } | PromptOrigin::Unknown => None,
    };
    data.record_trigger(&entry.tag(), RunTrigger::Steer { device });
}
