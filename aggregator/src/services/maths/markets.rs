//! Pure catalog-level ranking for `GET /markets`. Ranks on what a venue's own listing already
//! told us - price and reported volume - with no network calls, so a UI list view costs one
//! in-memory read.
//!
//! This is deliberately *not* a second copy of `maths::routing`. Those strategies rank real
//! quotes for a specific trade size and stay the authority on execution quality; these rank a
//! catalog. When a caller supplies `quote_size`, the route uses this to pick a shortlist cheaply
//! and then hands that shortlist to `maths::routing` for the real answer.

use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;

use crate::models::venue::VenueMarket;
use crate::repositories::market_repository::MarketSort;

/// Sorts `markets` in place under `sort`, ranking on `side`'s price where price matters.
///
/// Markets missing the field a sort ranks on always go last rather than being treated as zero:
/// an unpriced market is not "free" and a market with no reported volume is not "empty". Ties and
/// missing values fall back to closing-soonest so ordering is always total and stable, which
/// matters for pagination - an unstable comparator would shuffle rows between pages.
pub fn sort_markets(markets: &mut [&VenueMarket], sort: MarketSort, side: &str) {
    match sort {
        // A close_time of 0 means "no cutoff reported", which must sort last rather than first -
        // otherwise every open-ended market leads a "closing soonest" list.
        MarketSort::ClosingSoonest => {
            markets.sort_by_key(|m| (m.close_time == 0, m.close_time, m.venue_market_id.clone()))
        }
        MarketSort::BestPrice => markets.sort_by(|a, b| {
            // Option<Decimal>'s own Ord puts None first; we want it last, so compare as
            // (is_missing, value) and let `true` sort after `false`.
            let key = |m: &VenueMarket| (price_of(m, side).is_none(), price_of(m, side));
            key(a).cmp(&key(b)).then_with(|| close_key(a).cmp(&close_key(b)))
        }),
        MarketSort::BestLiquidity => markets.sort_by(|a, b| {
            let key = |m: &VenueMarket| (m.liquidity.is_none(), std::cmp::Reverse(m.liquidity));
            key(a).cmp(&key(b)).then_with(|| close_key(a).cmp(&close_key(b)))
        }),
        MarketSort::BestOverall => markets.sort_by(|a, b| {
            overall_score(b, side).cmp(&overall_score(a, side)).then_with(|| close_key(a).cmp(&close_key(b)))
        }),
    }
}

/// Tie-break key that keeps unknown close times (0) last rather than first.
fn close_key(market: &VenueMarket) -> (bool, i64) {
    (market.close_time == 0, market.close_time)
}

/// The requested side's catalog price, if the venue quoted one.
fn price_of(market: &VenueMarket, side: &str) -> Option<Decimal> {
    market.price_for(side).and_then(|raw| raw.parse().ok())
}

/// A blend of cheapness and depth, mirroring `maths::routing::score_best_overall`'s shape so the
/// catalog ordering and the live-quote ordering broadly agree: price dominates, depth breaks ties
/// on a log scale so a market with 100x the volume doesn't simply win outright.
///
/// A market missing either input scores very low rather than being excluded - it still appears,
/// just below everything we can actually rank.
fn overall_score(market: &VenueMarket, side: &str) -> Decimal {
    const MISSING_PENALTY: i64 = 1_000;

    let price_component = match price_of(market, side) {
        Some(price) => -price * Decimal::from(10),
        None => -Decimal::from(MISSING_PENALTY),
    };
    let liquidity_component = match market.liquidity {
        // ln(1 + volume), computed via checked_ln so a pathological non-positive volume can't
        // panic - same guard maths::routing uses.
        Some(volume) if volume > Decimal::ZERO => {
            (Decimal::ONE + volume).checked_ln().unwrap_or(Decimal::ZERO) * Decimal::new(1, 1)
        }
        _ => Decimal::ZERO,
    };
    price_component + liquidity_component
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::venue::Category;

    const NOW: i64 = 1_000_000;

    fn market(id: &str, yes_price: Option<&str>, liquidity: Option<&str>, close_time: i64) -> VenueMarket {
        VenueMarket {
            venue_id: "jupiter".to_string(),
            venue_market_id: id.to_string(),
            title: format!("Market {id}"),
            event: None,
            rules: None,
            category: Category::from("sports"),
            phase: "open".to_string(),
            start_time: NOW,
            close_time,
            resolution_time: close_time + 1_000,
            yes_price: yes_price.map(str::to_string),
            no_price: Some("0.5".to_string()),
            liquidity: liquidity.map(|l| l.parse().unwrap()),
            dead: false,
            ..Default::default()
        }
    }

    fn order(markets: &[VenueMarket], sort: MarketSort) -> Vec<String> {
        let mut refs: Vec<&VenueMarket> = markets.iter().collect();
        sort_markets(&mut refs, sort, "yes");
        refs.iter().map(|m| m.venue_market_id.clone()).collect()
    }

    #[test]
    fn best_price_puts_the_cheapest_first() {
        let markets = vec![
            market("expensive", Some("0.80"), None, NOW + 10),
            market("cheap", Some("0.20"), None, NOW + 10),
        ];
        assert_eq!(order(&markets, MarketSort::BestPrice)[0], "cheap");
    }

    #[test]
    fn best_price_sorts_unpriced_markets_last_not_as_free() {
        let markets = vec![
            market("unpriced", None, None, NOW + 10),
            market("priced", Some("0.90"), None, NOW + 10),
        ];
        // A missing price must not read as the best possible price.
        assert_eq!(order(&markets, MarketSort::BestPrice), vec!["priced", "unpriced"]);
    }

    #[test]
    fn best_liquidity_puts_the_deepest_first() {
        let markets = vec![
            market("thin", Some("0.5"), Some("100"), NOW + 10),
            market("deep", Some("0.5"), Some("900000"), NOW + 10),
        ];
        assert_eq!(order(&markets, MarketSort::BestLiquidity)[0], "deep");
    }

    #[test]
    fn best_liquidity_sorts_unknown_volume_last_not_as_zero() {
        let markets = vec![
            market("unknown", Some("0.5"), None, NOW + 10),
            market("zero", Some("0.5"), Some("0"), NOW + 10),
        ];
        assert_eq!(order(&markets, MarketSort::BestLiquidity), vec!["zero", "unknown"]);
    }

    #[test]
    fn closing_soonest_is_chronological() {
        let markets = vec![
            market("later", Some("0.5"), None, NOW + 500),
            market("sooner", Some("0.5"), None, NOW + 5),
        ];
        assert_eq!(order(&markets, MarketSort::ClosingSoonest), vec!["sooner", "later"]);
    }

    #[test]
    fn best_overall_prefers_cheap_and_deep_over_cheap_and_thin() {
        let markets = vec![
            market("cheap_thin", Some("0.30"), Some("1"), NOW + 10),
            market("cheap_deep", Some("0.30"), Some("500000"), NOW + 10),
        ];
        assert_eq!(order(&markets, MarketSort::BestOverall)[0], "cheap_deep");
    }

    #[test]
    fn best_overall_still_lets_price_dominate_depth() {
        let markets = vec![
            market("dear_deep", Some("0.95"), Some("1000000"), NOW + 10),
            market("cheap_thin", Some("0.10"), Some("10"), NOW + 10),
        ];
        assert_eq!(order(&markets, MarketSort::BestOverall)[0], "cheap_thin");
    }

    #[test]
    fn closing_soonest_puts_markets_with_no_reported_close_last() {
        let markets = vec![
            market("open_ended", Some("0.5"), None, 0),
            market("dated", Some("0.5"), None, NOW + 500),
        ];
        assert_eq!(order(&markets, MarketSort::ClosingSoonest), vec!["dated", "open_ended"]);
    }

    #[test]
    fn ordering_is_stable_enough_to_paginate() {
        // Same close_time and no ranking signal at all: order must still be deterministic, or
        // rows shuffle between pages.
        let markets = vec![market("b", None, None, NOW + 10), market("a", None, None, NOW + 10)];
        assert_eq!(order(&markets, MarketSort::ClosingSoonest), vec!["a", "b"]);
    }
}
