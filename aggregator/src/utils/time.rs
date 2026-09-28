//! Shared time helper, so every freshness and betting-window check reads the same clock.

use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
thread_local! {
    static FROZEN_NOW: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

/// Current time as Unix seconds.
pub fn now_unix() -> i64 {
    #[cfg(test)]
    if let Some(frozen) = FROZEN_NOW.with(|now| now.get()) {
        return frozen;
    }
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Pins `now_unix()` for the current thread, so tests over recorded venue data see the markets as
/// they were when captured instead of expiring as real time passes. Thread-local on purpose: tests
/// run in parallel, and a `current_thread` tokio runtime keeps its tasks on the test's thread.
#[cfg(test)]
pub fn freeze_now_for_this_thread(at: i64) {
    FROZEN_NOW.with(|now| now.set(Some(at)));
}
