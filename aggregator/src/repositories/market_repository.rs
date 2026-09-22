//! In-memory store for raw venue markets, keyed by (venue, category).
//!
//! This is the backing store for `GET /markets`, the markets-first discovery path. Unlike
//! `graph_repository`'s `CanonicalGraph` it does no matching, grouping or canonicalization - a
//! market goes in exactly as the venue reported it and comes out the same way. The canonical
//! event graph still exists alongside this and is fed by the same refresh; it earns its keep once
//! several venues quote the same event, which is not the situation today.
//!
//! Buckets are **replaced wholesale**, never appended to (`replace_bucket`). That is what keeps
//! memory bounded and stops resolved markets lingering forever - the failure mode the event graph
//! has, where `refresh()` only ever inserts.

use std::collections::HashMap;

use crate::models::venue::{Category, VenueMarket};

/// How a caller wants a market list ordered. Catalog-level only - these rank on what the venue's
/// own listing already told us, with no extra network calls. The live-quote strategies in
/// `services::maths::routing` take over when a caller supplies a trade size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketSort {
    /// Cheapest first on the requested side. Markets with no price sort last.
    BestPrice,
    /// Deepest first, by the venue's own reported volume. Markets with no volume sort last.
    BestLiquidity,
    /// A blend of cheapness and depth.
    BestOverall,
    /// Soonest betting cutoff first - the default.
    ClosingSoonest,
}

impl MarketSort {
    /// Parses the `sort` query param. `None` (absent) means `ClosingSoonest`; an unrecognized
    /// value is an error rather than a silent fallback, so a UI typo surfaces as a 400 instead of
    /// mysteriously ignored ordering.
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw {
            None | Some("") | Some("closing_soonest") => Ok(MarketSort::ClosingSoonest),
            Some("best_price") => Ok(MarketSort::BestPrice),
            Some("best_liquidity") => Ok(MarketSort::BestLiquidity),
            Some("best_execution") | Some("best_overall") => Ok(MarketSort::BestOverall),
            Some(other) => Err(format!(
                "unknown sort '{other}' - expected one of: closing_soonest, best_price, best_liquidity, best_execution, best_overall"
            )),
        }
    }
}

/// Every raw venue market seen on the most recent refresh, bucketed by venue and category.
#[derive(Debug, Default)]
pub struct MarketStore {
    buckets: HashMap<(String, String), Vec<VenueMarket>>,
    /// Unix seconds of the last completed bucket replacement, or `None` before the first refresh
    /// finishes. Surfaced on `/markets` so a UI can show data age rather than silently rendering
    /// a stale or empty catalog as if it were current.
    last_refreshed_at: Option<i64>,
}

impl MarketStore {
    /// An empty store, before anything has been fetched.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces everything held for one (venue, category) pair. Wholesale replacement is the
    /// point: a market the venue has stopped listing disappears here too, instead of accumulating.
    pub fn replace_bucket(&mut self, venue_id: &str, category: &Category, markets: Vec<VenueMarket>, now: i64) {
        let key = (venue_id.to_string(), String::from(category.clone()));
        if markets.is_empty() {
            self.buckets.remove(&key);
        } else {
            self.buckets.insert(key, markets);
        }
        self.last_refreshed_at = Some(now);
    }

    /// When the store was last written to.
    pub fn last_refreshed_at(&self) -> Option<i64> {
        self.last_refreshed_at
    }

    /// Total markets held, across every bucket.
    pub fn len(&self) -> usize {
        self.buckets.values().map(Vec::len).sum()
    }

    /// Markets in `category` that can still be bet on and whose betting window closes within
    /// `within_secs` of `now`, optionally narrowed to one venue.
    ///
    /// The window is on `close_time` — when the market stops accepting bets — not on
    /// `resolution_time`. Those are genuinely different moments on both integrated venues, and a
    /// market that has stopped taking bets is not something a UI should offer, however far off
    /// its settlement is.
    ///
    /// `now` and `within_secs` are plain parameters rather than read from the clock internally,
    /// so this stays pure and directly testable; the route owns clamping and supplying the clock.
    pub fn markets_in(
        &self,
        category: &Category,
        now: i64,
        within_secs: i64,
        venue: Option<&str>,
    ) -> Vec<&VenueMarket> {
        let wanted = String::from(category.clone());
        let horizon = now.saturating_add(within_secs);
        self.buckets
            .iter()
            .filter(|((bucket_venue, bucket_category), _)| {
                bucket_category == &wanted && venue.is_none_or(|v| bucket_venue == v)
            })
            .flat_map(|(_, markets)| markets.iter())
            .filter(|market| market.is_bettable(now))
            // A `close_time` of 0 means the venue reports no cutoff at all - Jupiter does this for
            // genuinely open-ended markets ("closes after a winner is declared"), 28 of 1063 on a
            // live sports page. Such a market cannot be asserted to close inside the window, so a
            // bounded query excludes it rather than padding results with false positives. It is
            // still `is_bettable`; it just can't satisfy a "closes within N days" contract.
            .filter(|market| market.close_time > 0 && market.close_time <= horizon)
            .collect()
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;
    const DAY: i64 = 86_400;

    fn market(id: &str, close_time: i64) -> VenueMarket {
        VenueMarket {
            venue_id: "jupiter".to_string(),
            venue_market_id: id.to_string(),
            title: format!("Market {id}"),
            event: None,
            rules: None,
            category: Category::Sports,
            phase: "open".to_string(),
            start_time: NOW - DAY,
            close_time,
            resolution_time: close_time + DAY,
            yes_price: Some("0.5".to_string()),
            no_price: Some("0.5".to_string()),
            liquidity: None,
            dead: false,
        }
    }

    fn store_with(markets: Vec<VenueMarket>) -> MarketStore {
        let mut store = MarketStore::new();
        store.replace_bucket("jupiter", &Category::Sports, markets, NOW);
        store
    }

    #[test]
    fn returns_a_market_closing_inside_the_window() {
        let store = store_with(vec![market("a", NOW + 3 * DAY)]);
        assert_eq!(store.markets_in(&Category::Sports, NOW, 7 * DAY, None).len(), 1);
    }

    #[test]
    fn excludes_a_market_closing_after_the_window() {
        let store = store_with(vec![market("a", NOW + 30 * DAY)]);
        assert!(store.markets_in(&Category::Sports, NOW, 7 * DAY, None).is_empty());
    }

    #[test]
    fn excludes_a_market_whose_betting_already_closed_though_it_has_not_resolved() {
        // close_time past, resolution_time far in the future - the case that motivated tracking
        // close_time separately in the first place.
        let mut m = market("a", NOW - DAY);
        m.resolution_time = NOW + 30 * DAY;
        let store = store_with(vec![m]);
        assert!(store.markets_in(&Category::Sports, NOW, 7 * DAY, None).is_empty());
    }

    #[test]
    fn excludes_a_dead_market() {
        let mut m = market("a", NOW + DAY);
        m.dead = true;
        let store = store_with(vec![m]);
        assert!(store.markets_in(&Category::Sports, NOW, 7 * DAY, None).is_empty());
    }

    #[test]
    fn a_different_category_does_not_leak_in() {
        let store = store_with(vec![market("a", NOW + DAY)]);
        assert!(store.markets_in(&Category::Crypto, NOW, 7 * DAY, None).is_empty());
    }

    #[test]
    fn venue_filter_narrows_results() {
        let store = store_with(vec![market("a", NOW + DAY)]);
        assert_eq!(store.markets_in(&Category::Sports, NOW, 7 * DAY, Some("jupiter")).len(), 1);
        assert!(store.markets_in(&Category::Sports, NOW, 7 * DAY, Some("panta")).is_empty());
    }

    #[test]
    fn replacing_a_bucket_drops_markets_the_venue_no_longer_lists() {
        let mut store = store_with(vec![market("a", NOW + DAY), market("b", NOW + DAY)]);
        assert_eq!(store.len(), 2);
        // Second refresh only sees one of them - the other must not linger.
        store.replace_bucket("jupiter", &Category::Sports, vec![market("a", NOW + DAY)], NOW + 60);
        assert_eq!(store.len(), 1);
        assert_eq!(store.markets_in(&Category::Sports, NOW, 7 * DAY, None)[0].venue_market_id, "a");
    }

    #[test]
    fn an_empty_refresh_clears_the_bucket_rather_than_keeping_stale_rows() {
        let mut store = store_with(vec![market("a", NOW + DAY)]);
        store.replace_bucket("jupiter", &Category::Sports, vec![], NOW + 60);
        assert_eq!(store.len(), 0);
        assert_eq!(store.last_refreshed_at(), Some(NOW + 60));
    }

    #[test]
    fn a_market_with_no_reported_close_time_is_excluded_from_a_bounded_window() {
        // Jupiter reports close_time 0 for open-ended markets. We can't claim those close within
        // the window, so a windowed query must not return them.
        let mut m = market("a", 0);
        m.resolution_time = NOW + 3 * DAY;
        let store = store_with(vec![m]);
        assert!(store.markets_in(&Category::Sports, NOW, 7 * DAY, None).is_empty());
    }

    #[test]
    fn sort_parsing_rejects_an_unknown_value() {
        assert_eq!(MarketSort::parse(None).unwrap(), MarketSort::ClosingSoonest);
        assert_eq!(MarketSort::parse(Some("best_price")).unwrap(), MarketSort::BestPrice);
        assert!(MarketSort::parse(Some("cheapest")).is_err());
    }
}
