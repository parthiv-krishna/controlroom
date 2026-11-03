//! The scheduler's wall clock: the system clock in production, a manual one in tests.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use controlroom_protocol::UnixMillis;
use tokio::sync::watch;

/// The longest the system clock sleeps before re-reading the time, so a wall-clock jump (NTP,
/// a suspended host) delays a due schedule by at most this much.
const MAX_SLEEP: Duration = Duration::from_secs(30);

/// The time source of the scheduler. Cheap to clone; the default is the system clock.
#[derive(Clone, Default)]
pub struct Clock(Option<Arc<watch::Sender<i64>>>);

impl fmt::Debug for Clock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            None => f.write_str("Clock::System"),
            Some(t) => write!(f, "Clock::Manual({})", *t.borrow()),
        }
    }
}

impl Clock {
    /// The system clock.
    pub fn system() -> Self {
        Self(None)
    }

    /// A clock that stands still at `start` until [`ManualClock::set`] moves it.
    pub fn manual(start: UnixMillis) -> (Self, ManualClock) {
        let (tx, _) = watch::channel(start.0);
        let tx = Arc::new(tx);
        (Self(Some(tx.clone())), ManualClock(tx))
    }

    /// The current time.
    pub(crate) fn now(&self) -> UnixMillis {
        match &self.0 {
            None => UnixMillis::now(),
            Some(t) => UnixMillis(*t.borrow()),
        }
    }

    /// Returns at `at` or later. It may also return early; callers re-read [`Self::now`].
    pub(crate) async fn sleep_until(&self, at: UnixMillis) {
        match &self.0 {
            None => {
                let left = u64::try_from(at.0.saturating_sub(UnixMillis::now().0)).unwrap_or(0);
                tokio::time::sleep(Duration::from_millis(left).min(MAX_SLEEP)).await;
            }
            Some(t) => {
                let mut rx = t.subscribe();
                // The sender lives as long as this clock, so `wait_for` only ends when it holds.
                let _ = rx.wait_for(|now| *now >= at.0).await;
            }
        }
    }
}

/// Moves a manual [`Clock`].
#[derive(Clone)]
pub struct ManualClock(Arc<watch::Sender<i64>>);

impl ManualClock {
    /// Sets the time (it may also go backwards).
    pub fn set(&self, at: UnixMillis) {
        self.0.send_replace(at.0);
    }

    /// Moves the time forward by `by`.
    pub fn advance(&self, by: Duration) {
        let by = i64::try_from(by.as_millis()).unwrap_or(i64::MAX);
        self.0.send_modify(|now| *now = now.saturating_add(by));
    }

    /// The current time.
    pub fn now(&self) -> UnixMillis {
        UnixMillis(*self.0.borrow())
    }
}
