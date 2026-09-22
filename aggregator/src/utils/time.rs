//! Shared time helper - was duplicated verbatim across `health_repository` and
//! `position_repository`; consolidated here so the event-window filter can use the same one.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current time as Unix seconds.
pub fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}
