//! In-memory store for raw venue markets, keyed by (venue, category).
//!
//! This is the source the `/v1` event graph is rebuilt from, and what is persisted. It does no
//! matching, grouping or canonicalization - a market goes in exactly as the venue reported it and
//! comes out the same way.
//!
//! Buckets are **replaced wholesale**, never appended to (`replace_bucket`). That is what keeps
//! memory bounded and stops resolved markets lingering forever.

use std::collections::HashMap;

use crate::models::venue::{Category, VenueMarket};

/// One (venue, category) bucket and how current it is.
#[derive(Debug)]
struct Bucket {
    markets: Vec<VenueMarket>,
    /// Unix seconds of the last *complete* refresh of this bucket.
    refreshed_at: i64,
    /// True when the most recent refresh attempt failed and `markets` is the last good copy.
    stale: bool,
}

/// Freshness of one bucket, for metrics and the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketStatus {
    pub venue_id: String,
    pub category: String,
    pub markets: usize,
    pub refreshed_at: i64,
    pub stale: bool,
}

/// One shared category and which venues hold markets under it, in their own words.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct CategoryBreakdown {
    pub category: String,
    pub markets: usize,
    pub venues: Vec<VenueCategoryNames>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct VenueCategoryNames {
    pub venue_id: String,
    /// The venue's own category names that map to this one, verbatim.
    pub native: Vec<String>,
    pub markets: usize,
}

/// Every raw venue market seen on the most recent refresh, bucketed by venue and category.
#[derive(Debug, Default)]
pub struct MarketStore {
    buckets: HashMap<(String, String), Bucket>,
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
            self.buckets.insert(key, Bucket { markets, refreshed_at: now, stale: false });
        }
    }

    /// Replaces *every* bucket held for `venue_id` at once. For a venue without a server-side
    /// category filter, one complete chain is the whole catalog, so a category that no longer
    /// appears in it must disappear too rather than linger from an earlier refresh.
    pub fn replace_venue(&mut self, venue_id: &str, by_category: Vec<(Category, Vec<VenueMarket>)>, now: i64) {
        self.buckets.retain(|(bucket_venue, _), _| bucket_venue != venue_id);
        for (category, markets) in by_category {
            self.replace_bucket(venue_id, &category, markets, now);
        }
    }

    /// Records that a refresh of this venue failed. The markets already held are kept - a failed
    /// refresh is not evidence the venue stopped listing them - but flagged stale so the API and
    /// metrics can say so. Returns how many markets were kept.
    pub fn mark_venue_stale(&mut self, venue_id: &str) -> usize {
        self.buckets
            .iter_mut()
            .filter(|((bucket_venue, _), _)| bucket_venue == venue_id)
            .map(|(_, bucket)| {
                bucket.stale = true;
                bucket.markets.len()
            })
            .sum()
    }

    /// Every market held, across all buckets.
    pub fn all_markets(&self) -> Vec<VenueMarket> {
        self.buckets.values().flat_map(|bucket| bucket.markets.iter().cloned()).collect()
    }

    /// The venue's own category names for every market held for `venue_id`, sorted and distinct.
    /// The refresh falls back to these when a venue can't list its categories.
    pub fn venue_categories_held(&self, venue_id: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .buckets
            .iter()
            .filter(|((bucket_venue, _), _)| bucket_venue == venue_id)
            .flat_map(|(_, bucket)| bucket.markets.iter().map(|m| m.venue_category.clone()))
            .filter(|name| !name.trim().is_empty())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Every category held, with each venue's own names for it and market counts - the data
    /// behind `GET /categories`.
    pub fn category_breakdown(&self) -> Vec<CategoryBreakdown> {
        let mut by_category: std::collections::BTreeMap<String, std::collections::BTreeMap<String, (usize, std::collections::BTreeSet<String>)>> =
            std::collections::BTreeMap::new();
        for ((venue_id, category), bucket) in &self.buckets {
            let entry = by_category.entry(category.clone()).or_default().entry(venue_id.clone()).or_default();
            entry.0 += bucket.markets.len();
            entry.1.extend(bucket.markets.iter().map(|m| m.venue_category.clone()).filter(|n| !n.is_empty()));
        }
        by_category
            .into_iter()
            .map(|(category, venues)| CategoryBreakdown {
                markets: venues.values().map(|(n, _)| n).sum(),
                category,
                venues: venues
                    .into_iter()
                    .map(|(venue_id, (markets, native))| VenueCategoryNames { venue_id, native: native.into_iter().collect(), markets })
                    .collect(),
            })
            .collect()
    }

    /// Freshness of every bucket held.
    pub fn bucket_statuses(&self) -> Vec<BucketStatus> {
        let mut statuses: Vec<BucketStatus> = self
            .buckets
            .iter()
            .map(|((venue_id, category), bucket)| BucketStatus {
                venue_id: venue_id.clone(),
                category: category.clone(),
                markets: bucket.markets.len(),
                refreshed_at: bucket.refreshed_at,
                stale: bucket.stale,
            })
            .collect();
        statuses.sort_by(|a, b| (&a.venue_id, &a.category).cmp(&(&b.venue_id, &b.category)));
        statuses
    }

    /// Total markets held, across every bucket.
    pub fn len(&self) -> usize {
        self.buckets.values().map(|bucket| bucket.markets.len()).sum()
    }

    #[cfg(test)]
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
            .flat_map(|(_, bucket)| bucket.markets.iter())
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
            category: Category::from("sports"),
            phase: "open".to_string(),
            start_time: NOW - DAY,
            close_time,
            resolution_time: close_time + DAY,
            yes_price: Some("0.5".to_string()),
            no_price: Some("0.5".to_string()),
            liquidity: None,
            dead: false,
            ..Default::default()
        }
    }

    fn store_with(markets: Vec<VenueMarket>) -> MarketStore {
        let mut store = MarketStore::new();
        store.replace_bucket("jupiter", &Category::from("sports"), markets, NOW);
        store
    }

    #[test]
    fn returns_a_market_closing_inside_the_window() {
        let store = store_with(vec![market("a", NOW + 3 * DAY)]);
        assert_eq!(store.markets_in(&Category::from("sports"), NOW, 7 * DAY, None).len(), 1);
    }

    #[test]
    fn excludes_a_market_closing_after_the_window() {
        let store = store_with(vec![market("a", NOW + 30 * DAY)]);
        assert!(store.markets_in(&Category::from("sports"), NOW, 7 * DAY, None).is_empty());
    }

    #[test]
    fn excludes_a_market_whose_betting_already_closed_though_it_has_not_resolved() {
        // close_time past, resolution_time far in the future - the case that motivated tracking
        // close_time separately in the first place.
        let mut m = market("a", NOW - DAY);
        m.resolution_time = NOW + 30 * DAY;
        let store = store_with(vec![m]);
        assert!(store.markets_in(&Category::from("sports"), NOW, 7 * DAY, None).is_empty());
    }

    #[test]
    fn excludes_a_dead_market() {
        let mut m = market("a", NOW + DAY);
        m.dead = true;
        let store = store_with(vec![m]);
        assert!(store.markets_in(&Category::from("sports"), NOW, 7 * DAY, None).is_empty());
    }

    #[test]
    fn a_different_category_does_not_leak_in() {
        let store = store_with(vec![market("a", NOW + DAY)]);
        assert!(store.markets_in(&Category::from("crypto"), NOW, 7 * DAY, None).is_empty());
    }

    #[test]
    fn venue_filter_narrows_results() {
        let store = store_with(vec![market("a", NOW + DAY)]);
        assert_eq!(store.markets_in(&Category::from("sports"), NOW, 7 * DAY, Some("jupiter")).len(), 1);
        assert!(store.markets_in(&Category::from("sports"), NOW, 7 * DAY, Some("panta")).is_empty());
    }

    #[test]
    fn replacing_a_bucket_drops_markets_the_venue_no_longer_lists() {
        let mut store = store_with(vec![market("a", NOW + DAY), market("b", NOW + DAY)]);
        assert_eq!(store.len(), 2);
        // Second refresh only sees one of them - the other must not linger.
        store.replace_bucket("jupiter", &Category::from("sports"), vec![market("a", NOW + DAY)], NOW + 60);
        assert_eq!(store.len(), 1);
        assert_eq!(store.markets_in(&Category::from("sports"), NOW, 7 * DAY, None)[0].venue_market_id, "a");
    }

    #[test]
    fn an_empty_refresh_clears_the_bucket_rather_than_keeping_stale_rows() {
        let mut store = store_with(vec![market("a", NOW + DAY)]);
        store.replace_bucket("jupiter", &Category::from("sports"), vec![], NOW + 60);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn a_market_with_no_reported_close_time_is_excluded_from_a_bounded_window() {
        // Jupiter reports close_time 0 for open-ended markets. We can't claim those close within
        // the window, so a windowed query must not return them.
        let mut m = market("a", 0);
        m.resolution_time = NOW + 3 * DAY;
        let store = store_with(vec![m]);
        assert!(store.markets_in(&Category::from("sports"), NOW, 7 * DAY, None).is_empty());
    }

    #[test]
    fn marking_a_bucket_stale_keeps_its_markets() {
        let mut store = store_with(vec![market("a", NOW + DAY)]);
        assert_eq!(store.mark_venue_stale("jupiter"), 1);
        assert_eq!(store.len(), 1);
        let status = &store.bucket_statuses()[0];
        assert!(status.stale);
        assert_eq!(status.refreshed_at, NOW);
    }

    #[test]
    fn a_successful_replace_clears_staleness() {
        let mut store = store_with(vec![market("a", NOW + DAY)]);
        store.mark_venue_stale("jupiter");
        store.replace_bucket("jupiter", &Category::from("sports"), vec![market("b", NOW + DAY)], NOW + 60);
        let status = &store.bucket_statuses()[0];
        assert!(!status.stale);
        assert_eq!(status.refreshed_at, NOW + 60);
    }

    #[test]
    fn replacing_a_venue_drops_categories_it_no_longer_lists() {
        let mut store = store_with(vec![market("a", NOW + DAY)]);
        store.replace_bucket("jupiter", &Category::from("crypto"), vec![market("b", NOW + DAY)], NOW);
        store.replace_bucket("panta", &Category::from("crypto"), vec![market("c", NOW + DAY)], NOW);
        store.replace_venue("jupiter", vec![(Category::from("crypto"), vec![market("b", NOW + DAY)])], NOW + 60);
        assert!(store.markets_in(&Category::from("sports"), NOW, 7 * DAY, None).is_empty());
        assert_eq!(store.markets_in(&Category::from("crypto"), NOW, 7 * DAY, None).len(), 2);
    }
}
