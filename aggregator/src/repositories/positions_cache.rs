//! A seconds-long cache of a wallet's positions.
//!
//! A portfolio page refreshes on a timer and every open tab does it, and each read fans out to
//! every venue with our API key. The TTL is short enough that a fill shows up promptly and long
//! enough to absorb a burst.
//!
//! Only a **complete** result is stored. A response where a venue failed is never cached: serving a
//! partial list from cache would keep showing a wallet as missing positions well after the venue
//! recovered.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::models::position::PositionsResponse;

/// How long a complete result is reused.
const TTL: Duration = Duration::from_secs(8);

/// Wallets held before expired ones are swept; if a sweep doesn't get under this, the cache is
/// cleared. Bounds memory against a caller cycling through wallets.
const MAX_ENTRIES: usize = 2_000;

#[derive(Default)]
pub struct PositionsCache {
    entries: Mutex<HashMap<String, (Instant, PositionsResponse)>>,
}

impl PositionsCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh complete result for this wallet, if one is held.
    pub fn get(&self, wallet: &str) -> Option<PositionsResponse> {
        self.get_at(wallet, Instant::now())
    }

    /// Stores `response`, unless it is incomplete.
    pub fn insert(&self, response: &PositionsResponse) {
        self.insert_at(response, Instant::now());
    }

    fn get_at(&self, wallet: &str, now: Instant) -> Option<PositionsResponse> {
        let entries = self.entries.lock().expect("positions cache lock poisoned");
        entries.get(wallet).filter(|(at, _)| now.duration_since(*at) < TTL).map(|(_, r)| r.clone())
    }

    fn insert_at(&self, response: &PositionsResponse, now: Instant) {
        if !response.venues_failed.is_empty() {
            return;
        }
        let mut entries = self.entries.lock().expect("positions cache lock poisoned");
        if entries.len() >= MAX_ENTRIES {
            entries.retain(|_, (at, _)| now.duration_since(*at) < TTL);
            if entries.len() >= MAX_ENTRIES {
                entries.clear();
            }
        }
        entries.insert(response.wallet.clone(), (now, response.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorCode;
    use crate::models::position::VenueFailure;

    fn response(wallet: &str, failed: bool) -> PositionsResponse {
        PositionsResponse {
            wallet: wallet.to_string(),
            positions: vec![],
            venues_failed: if failed {
                vec![VenueFailure { venue_id: "jupiter".into(), code: ErrorCode::VenueUnavailable, message: "down".into() }]
            } else {
                vec![]
            },
            venues_ok: vec!["panta".into()],
            fetched_at: 0,
        }
    }

    #[test]
    fn a_complete_result_is_served_until_it_expires() {
        let cache = PositionsCache::new();
        let t0 = Instant::now();
        cache.insert_at(&response("W", false), t0);
        assert!(cache.get_at("W", t0 + Duration::from_secs(7)).is_some());
        assert!(cache.get_at("W", t0 + Duration::from_secs(9)).is_none());
        assert!(cache.get_at("OTHER", t0).is_none(), "wallets never share an entry");
    }

    #[test]
    fn an_incomplete_result_is_never_cached() {
        let cache = PositionsCache::new();
        let t0 = Instant::now();
        cache.insert_at(&response("W", true), t0);
        assert!(cache.get_at("W", t0).is_none(), "a venue that failed must be retried, not remembered as empty");
    }

    #[test]
    fn the_cache_stays_bounded() {
        let cache = PositionsCache::new();
        let t0 = Instant::now();
        for n in 0..(MAX_ENTRIES + 10) {
            cache.insert_at(&response(&format!("W{n}"), false), t0);
        }
        assert!(cache.entries.lock().unwrap().len() <= MAX_ENTRIES);
    }
}
