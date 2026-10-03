//! Our ratings: how good an event is to look at, and which market inside it to look at first.
//!
//! Both are 0-100 with the breakdown returned, so the order is explainable. A component the data
//! can't provide is left out and the remaining weights renormalised, so a market isn't penalised
//! for what its venue doesn't report.

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::Serialize;
use utoipa::ToSchema;

use crate::models::venue::VenueMarket;

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

/// Combines `(name, 0-100 score, weight)` parts into a rating, renormalising the weights.
fn combine(parts: Vec<(&'static str, f64, f64)>) -> Rating {
    let total: f64 = parts.iter().map(|(_, _, w)| w).sum();
    let score = if total > 0.0 { parts.iter().map(|(_, s, w)| s * w).sum::<f64>() / total } else { 0.0 };
    Rating {
        score: score.round().clamp(0.0, 100.0) as u8,
        breakdown: parts
            .into_iter()
            .map(|(name, s, w)| RatingComponent { name, score: s.round().clamp(0.0, 100.0) as u8, weight: if total > 0.0 { w / total } else { 0.0 } })
            .collect(),
    }
}

fn log_share(value: Decimal, max: Decimal) -> f64 {
    let (v, m) = (value.to_f64().unwrap_or(0.0).max(0.0), max.to_f64().unwrap_or(0.0).max(0.0));
    if m <= 0.0 { 0.0 } else { 100.0 * (1.0 + v).ln() / (1.0 + m).ln() }
}

/// A market's volume as the venue reported it (`liquidity` carries the venue's volume figure).
pub fn volume_of(market: &VenueMarket) -> Decimal {
    market.liquidity.unwrap_or(Decimal::ZERO)
}

/// Rates one market among its event's markets: how much is traded on it (`activity`), how tight its
/// two prices are (`quality`: a YES and NO that sum to far more than 1 is a wide spread), and
/// whether it can be traded at all through a healthy venue (`reliability`).
pub fn rate_market(market: &VenueMarket, max_volume_in_event: Decimal, venue_routable: bool) -> Rating {
    let mut parts: Vec<(&'static str, f64, f64)> = Vec::new();
    if max_volume_in_event > Decimal::ZERO {
        parts.push(("activity", log_share(volume_of(market), max_volume_in_event), 0.5));
    }
    let yes = market.yes_price.as_deref().and_then(|p| p.parse::<f64>().ok());
    let no = market.no_price.as_deref().and_then(|p| p.parse::<f64>().ok());
    if let (Some(yes), Some(no)) = (yes, no) {
        // Each cent the two sides overshoot 1.00 by costs 10 points of quality.
        parts.push(("quality", (100.0 - ((yes + no) - 1.0).abs() * 1000.0).clamp(0.0, 100.0), 0.3));
    }
    parts.push(("reliability", if !market.display_only && venue_routable { 100.0 } else { 0.0 }, 0.2));
    combine(parts)
}

/// Rates an event: how much is traded across it (`activity`), how many venues list it (`coverage`:
/// a second venue is worth a comparison, which is what an aggregator adds), and how many markets
/// it holds (`depth`).
pub fn rate_event(volume: Decimal, max_volume: Decimal, venue_count: usize, market_count: usize) -> Rating {
    let coverage = match venue_count {
        0 => 0.0,
        1 => 40.0,
        _ => 100.0,
    };
    let depth = (100.0 * (market_count as f64).ln_1p() / 10f64.ln_1p()).clamp(0.0, 100.0);
    combine(vec![("activity", log_share(volume, max_volume), 0.5), ("coverage", coverage, 0.3), ("depth", depth, 0.2)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market(yes: &str, no: &str, volume: Option<u32>) -> VenueMarket {
        VenueMarket {
            yes_price: Some(yes.into()),
            no_price: Some(no.into()),
            liquidity: volume.map(Decimal::from),
            ..Default::default()
        }
    }

    #[test]
    fn a_busier_market_with_tight_prices_rates_higher() {
        let max = Decimal::from(1000);
        let busy = rate_market(&market("0.50", "0.50", Some(1000)), max, true);
        let quiet = rate_market(&market("0.50", "0.50", Some(10)), max, true);
        let wide = rate_market(&market("0.60", "0.60", Some(1000)), max, true);
        assert!(busy.score > quiet.score);
        assert!(busy.score > wide.score);
    }

    #[test]
    fn an_untradable_or_unhealthy_market_is_marked_down() {
        let max = Decimal::from(1000);
        let up = rate_market(&market("0.5", "0.5", Some(1000)), max, true);
        let down = rate_market(&market("0.5", "0.5", Some(1000)), max, false);
        assert!(up.score > down.score);
    }

    #[test]
    fn a_missing_component_is_left_out_not_scored_zero() {
        let rating = rate_market(&VenueMarket::default(), Decimal::ZERO, true);
        assert!(rating.breakdown.iter().all(|c| c.name != "activity" && c.name != "quality"));
        assert_eq!(rating.score, 100);
        let weights: f64 = rating.breakdown.iter().map(|c| c.weight).sum();
        assert!((weights - 1.0).abs() < 1e-9);
    }

    #[test]
    fn an_event_on_two_venues_outrates_the_same_event_on_one() {
        let max = Decimal::from(1000);
        let both = rate_event(Decimal::from(500), max, 2, 3);
        let one = rate_event(Decimal::from(500), max, 1, 3);
        assert!(both.score > one.score);
    }
}
