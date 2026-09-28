//! The listing rating: which venue is the better place to back one outcome.
//!
//! Only listings of the **same outcome** are compared - never unrelated markets (audit A4). Each
//! listing gets a 0-100 score from the components its venue can provide, weighted by config and
//! returned with its breakdown so the order is explainable. A component a venue can't provide is
//! left out and the remaining weights renormalised, so a venue isn't penalised for its API's shape.
//!
//! Catalog components (every refresh): price, activity, venue reliability. Live components
//! (fillability, depth at a size) are added when a caller asks with `quote_size`.

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::Serialize;
use utoipa::ToSchema;

use super::graph::Listing;

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct RatingWeights {
    pub price: f64,
    pub activity: f64,
    pub reliability: f64,
}

impl Default for RatingWeights {
    /// The plan's MVP weights (price 40, activity 10, reliability 10) for the components the
    /// catalog has, renormalised when a component is missing.
    fn default() -> Self {
        Self { price: 0.40, activity: 0.10, reliability: 0.10 }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Rating {
    /// 0-100.
    pub score: u8,
    pub breakdown: Vec<RatingComponent>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RatingComponent {
    pub name: &'static str,
    /// 0-100 for this component alone.
    pub score: u8,
    /// Share of the total after renormalisation, 0-1.
    pub weight: f64,
}

/// Rates `listings` (all backing one outcome) in place and sorts them best first.
/// `routable` answers whether a venue is currently healthy; a display-only listing scores as unroutable.
pub fn rate_outcome(listings: &mut [Listing], weights: &RatingWeights, routable: impl Fn(&str) -> bool) {
    let prices: Vec<f64> = listings.iter().filter_map(|l| l.price.and_then(|p| p.to_f64())).collect();
    let best_price = prices.iter().copied().fold(f64::INFINITY, f64::min);
    let volumes: Vec<f64> = listings.iter().filter_map(|l| l.volume_usdc.and_then(|v| v.to_f64())).collect();
    let max_log_volume = volumes.iter().map(|v| (1.0 + v.max(0.0)).ln()).fold(0.0, f64::max);

    for listing in listings.iter_mut() {
        let mut parts: Vec<(&'static str, f64, f64)> = Vec::new();
        if let Some(price) = listing.price.and_then(|p| p.to_f64()) {
            // Each cent above the best price for this outcome costs 5 points.
            parts.push(("price", (100.0 - (price - best_price) * 500.0).clamp(0.0, 100.0), weights.price));
        }
        if let Some(volume) = listing.volume_usdc.and_then(|v| v.to_f64())
            && max_log_volume > 0.0
        {
            parts.push(("activity", 100.0 * (1.0 + volume.max(0.0)).ln() / max_log_volume, weights.activity));
        }
        parts.push(("reliability", if listing.tradable && routable(&listing.venue_id) { 100.0 } else { 0.0 }, weights.reliability));

        let total_weight: f64 = parts.iter().map(|(_, _, w)| w).sum();
        let score = if total_weight > 0.0 { parts.iter().map(|(_, s, w)| s * w).sum::<f64>() / total_weight } else { 0.0 };
        listing.rating = Some(Rating {
            score: score.round() as u8,
            breakdown: parts
                .into_iter()
                .map(|(name, s, w)| RatingComponent { name, score: s.round() as u8, weight: if total_weight > 0.0 { w / total_weight } else { 0.0 } })
                .collect(),
        });
    }
    listings.sort_by(|a, b| {
        let score = |l: &Listing| l.rating.as_ref().map_or(0, |r| r.score);
        score(b).cmp(&score(a)).then_with(|| a.price.unwrap_or(Decimal::ONE).cmp(&b.price.unwrap_or(Decimal::ONE)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(venue: &str, price: &str, volume: Option<&str>) -> Listing {
        Listing {
            venue_id: venue.into(),
            venue_market_id: format!("{venue}-m"),
            venue_event_id: None,
            side: "yes".into(),
            price: Some(price.parse().unwrap()),
            volume_usdc: volume.map(|v| v.parse().unwrap()),
            close_time: 0,
            tradable: true,
            rules: None,
            rating: None,
        }
    }

    #[test]
    fn the_cheaper_listing_for_the_same_outcome_rates_higher() {
        let mut listings = vec![listing("a", "0.60", Some("1000")), listing("b", "0.55", Some("1000"))];
        rate_outcome(&mut listings, &RatingWeights::default(), |_| true);
        assert_eq!(listings[0].venue_id, "b");
        assert!(listings[0].rating.as_ref().unwrap().score > listings[1].rating.as_ref().unwrap().score);
    }

    #[test]
    fn an_unhealthy_venue_is_marked_down() {
        let mut listings = vec![listing("down", "0.50", None), listing("up", "0.51", None)];
        rate_outcome(&mut listings, &RatingWeights::default(), |v| v == "up");
        assert_eq!(listings[0].venue_id, "up");
    }

    #[test]
    fn a_missing_component_is_left_out_not_scored_zero() {
        let mut listings = vec![listing("a", "0.50", None)];
        rate_outcome(&mut listings, &RatingWeights::default(), |_| true);
        let rating = listings[0].rating.as_ref().unwrap();
        assert_eq!(rating.score, 100);
        assert!(rating.breakdown.iter().all(|c| c.name != "activity"));
        let weights: f64 = rating.breakdown.iter().map(|c| c.weight).sum();
        assert!((weights - 1.0).abs() < 1e-9);
    }
}
