//! A short-lived cache of live venue quotes.
//!
//! Every `/markets?quote_size=` page and every `/find-best-market` call fans out to the venues.
//! Without a cache, repeated or concurrent requests for the same market and size each cost a
//! venue call, which is how anonymous traffic burns our rate limit (audit A5). Prices move, so the
//! TTL is seconds, not minutes: long enough to absorb a burst, short enough that a quote is still
//! a quote.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rust_decimal::Decimal;

use crate::models::venue::VenueQuote;

/// How long a quote is reused.
const TTL: Duration = Duration::from_secs(5);

/// Entries held before expired ones are swept; if a sweep doesn't get under this, the cache is
/// cleared. Bounds memory against a caller cycling through sizes.
const MAX_ENTRIES: usize = 5_000;

type Key = (&'static str, String, String, Decimal);

#[derive(Default)]
pub struct QuoteCache {
    entries: Mutex<HashMap<Key, (Instant, VenueQuote)>>,
}

impl QuoteCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// A cached quote for this exact (venue, market, side, size), if one is still fresh.
    pub fn get(&self, venue_id: &'static str, market_id: &str, side: &str, size: Decimal) -> Option<VenueQuote> {
        self.get_at(venue_id, market_id, side, size, Instant::now())
    }

    pub fn insert(&self, venue_id: &'static str, market_id: &str, side: &str, size: Decimal, quote: VenueQuote) {
        self.insert_at(venue_id, market_id, side, size, quote, Instant::now());
    }

    fn get_at(&self, venue_id: &'static str, market_id: &str, side: &str, size: Decimal, now: Instant) -> Option<VenueQuote> {
        let key = (venue_id, market_id.to_string(), side.to_string(), size.normalize());
        let entries = self.entries.lock().expect("quote cache lock poisoned");
        entries.get(&key).filter(|(at, _)| now.duration_since(*at) < TTL).map(|(_, quote)| quote.clone())
    }

    fn insert_at(&self, venue_id: &'static str, market_id: &str, side: &str, size: Decimal, quote: VenueQuote, now: Instant) {
        let mut entries = self.entries.lock().expect("quote cache lock poisoned");
        if entries.len() >= MAX_ENTRIES {
            entries.retain(|_, (at, _)| now.duration_since(*at) < TTL);
            if entries.len() >= MAX_ENTRIES {
                entries.clear();
            }
        }
        entries.insert((venue_id, market_id.to_string(), side.to_string(), size.normalize()), (now, quote));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote() -> VenueQuote {
        VenueQuote {
            venue: "jupiter",
            venue_market_id: "m".to_string(),
            side: "yes".to_string(),
            amount_usdc: Decimal::from(10),
            effective_shares_or_size: Decimal::from(20),
            avg_price: Decimal::new(5, 1),
            fee_usdc: Decimal::ZERO,
            fully_filled: true,
            expires_at: None,
        }
    }

    #[test]
    fn a_fresh_quote_is_reused_and_an_expired_one_is_not() {
        let cache = QuoteCache::new();
        let t0 = Instant::now();
        cache.insert_at("jupiter", "m", "yes", Decimal::from(10), quote(), t0);
        assert!(cache.get_at("jupiter", "m", "yes", Decimal::from(10), t0 + Duration::from_secs(1)).is_some());
        assert!(cache.get_at("jupiter", "m", "yes", Decimal::from(10), t0 + TTL).is_none());
    }

    #[test]
    fn equal_sizes_written_differently_share_an_entry() {
        let cache = QuoteCache::new();
        let t0 = Instant::now();
        cache.insert_at("jupiter", "m", "yes", "10.00".parse().unwrap(), quote(), t0);
        assert!(cache.get_at("jupiter", "m", "yes", Decimal::from(10), t0).is_some());
    }

    #[test]
    fn a_different_side_or_size_misses() {
        let cache = QuoteCache::new();
        let t0 = Instant::now();
        cache.insert_at("jupiter", "m", "yes", Decimal::from(10), quote(), t0);
        assert!(cache.get_at("jupiter", "m", "no", Decimal::from(10), t0).is_none());
        assert!(cache.get_at("jupiter", "m", "yes", Decimal::from(11), t0).is_none());
    }
}
