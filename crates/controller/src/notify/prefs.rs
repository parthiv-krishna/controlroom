//! The owner's notification preferences, stored server-side (one row each, so setting one never
//! overwrites another).

use controlroom_protocol::ui::{NotificationPreferences, NotificationsSetPreferencesParams};

use crate::store::{Prefs, StoreResult};

const ATTENTION: &str = "notifications.attention";
const QUESTIONS: &str = "notifications.questions";
const TURN_END: &str = "notifications.turn_end";
const HOLD_WHILE_ACTIVE: &str = "notifications.hold_while_active";
const DELAY_S: &str = "notifications.delay_s";
const IDLE_S: &str = "notifications.idle_s";

/// How long a push waits by default (seconds).
const DEFAULT_DELAY_S: u32 = 30;
/// The longest push delay (seconds).
const MAX_DELAY_S: u32 = 3600;
/// How long after the last input a device stops being active by default (seconds).
const DEFAULT_IDLE_S: u32 = 120;
/// The idle window's bounds (seconds): well above the clients' input heartbeat (~20 s).
const IDLE_RANGE_S: std::ops::RangeInclusive<u32> = 60..=3600;

/// The owner's preferences. By default approvals and problems push, questions and finished runs
/// are in-app only, and pushes wait 30 s and are held while the user is active (2 min idle
/// window).
pub(crate) fn load(prefs: &Prefs<'_>) -> StoreResult<NotificationPreferences> {
    Ok(NotificationPreferences {
        attention: prefs.get(ATTENTION)?.unwrap_or(true),
        questions: prefs.get(QUESTIONS)?.unwrap_or(false),
        turn_end: prefs.get(TURN_END)?.unwrap_or(false),
        hold_while_active: prefs.get(HOLD_WHILE_ACTIVE)?.unwrap_or(true),
        delay_s: prefs.get(DELAY_S)?.unwrap_or(DEFAULT_DELAY_S),
        idle_s: prefs.get(IDLE_S)?.unwrap_or(DEFAULT_IDLE_S),
    })
}

/// Why a change is out of range, if it is.
pub(crate) fn check(p: &NotificationsSetPreferencesParams) -> Result<(), String> {
    if p.delay_s.is_some_and(|s| s > MAX_DELAY_S) {
        return Err(format!("delay_s must be at most {MAX_DELAY_S}"));
    }
    if p.idle_s.is_some_and(|s| !IDLE_RANGE_S.contains(&s)) {
        return Err(format!(
            "idle_s must be between {} and {}",
            IDLE_RANGE_S.start(),
            IDLE_RANGE_S.end()
        ));
    }
    Ok(())
}

/// Sets the given preferences only (checked with [`check`] first).
pub(crate) fn set(prefs: &Prefs<'_>, p: &NotificationsSetPreferencesParams) -> StoreResult<()> {
    for (key, value) in [
        (ATTENTION, p.attention),
        (QUESTIONS, p.questions),
        (TURN_END, p.turn_end),
        (HOLD_WHILE_ACTIVE, p.hold_while_active),
    ] {
        if let Some(value) = value {
            prefs.set(key, &value)?;
        }
    }
    for (key, value) in [(DELAY_S, p.delay_s), (IDLE_S, p.idle_s)] {
        if let Some(value) = value {
            prefs.set(key, &value)?;
        }
    }
    Ok(())
}
