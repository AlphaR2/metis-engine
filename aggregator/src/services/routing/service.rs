//! The smart router: given a canonical outcome and a trade size, queries every venue backing it,
//! scores the candidates under a pluggable `RoutingStrategy`, and returns a `Recommendation` —
//! the actual `find_best_market(event, outcome, size, strategy)` primitive from the design doc.
//! Data shapes (`RoutingCandidate`, `ScoredCandidate`, `Recommendation`) live in
//! `crate::models::routing`; the actual scoring math lives in `crate::services::maths::routing`;
//! this module is the orchestration in between.

use rust_decimal::Decimal;

use crate::errors::RoutingError;
use crate::models::event::VenueMarketRef;
use crate::models::routing::{Recommendation, RoutingCandidate, ScoredCandidate};
use crate::repositories::health_repository::HealthRegistry;
use crate::services::VenueAdapter;
use crate::services::maths::routing as maths;

/// A pluggable way to rank venue candidates - price, liquidity, or a blend.
pub trait RoutingStrategy: Send + Sync {
    /// Ranks `candidates` best-first. Implementations should treat an unroutable (unhealthy)
    /// candidate as always-worse than a routable one, regardless of its price.
    fn rank(&self, candidates: &[RoutingCandidate]) -> Vec<ScoredCandidate>;
}

/// Ranks purely by lowest price.
pub struct BestPrice;
impl RoutingStrategy for BestPrice {
    fn rank(&self, candidates: &[RoutingCandidate]) -> Vec<ScoredCandidate> {
        maths::score_best_price(candidates)
    }
}

/// Ranks by how much size a venue can actually fill.
pub struct BestLiquidity;
impl RoutingStrategy for BestLiquidity {
    fn rank(&self, candidates: &[RoutingCandidate]) -> Vec<ScoredCandidate> {
        maths::score_best_liquidity(candidates)
    }
}

/// Ranks by whether the full requested size can be filled at all.
pub struct BestExecution;
impl RoutingStrategy for BestExecution {
    fn rank(&self, candidates: &[RoutingCandidate]) -> Vec<ScoredCandidate> {
        maths::score_best_execution(candidates)
    }
}

/// The default: a weighted blend of price, liquidity, and fillability.
pub struct BestOverall;
impl RoutingStrategy for BestOverall {
    fn rank(&self, candidates: &[RoutingCandidate]) -> Vec<ScoredCandidate> {
        maths::score_best_overall(candidates)
    }
}

/// Picks a strategy by its name string. Absent means `BestOverall`; an unrecognised name is an
/// error, matching `/markets`' `sort`, so a typo surfaces as a 400 instead of silently ranking by
/// something the caller didn't ask for.
pub fn strategy_by_name(name: Option<&str>) -> Result<Box<dyn RoutingStrategy>, String> {
    match name {
        None | Some("") | Some("best_overall") => Ok(Box::new(BestOverall)),
        Some("best_price") => Ok(Box::new(BestPrice)),
        Some("best_liquidity") => Ok(Box::new(BestLiquidity)),
        Some("best_execution") => Ok(Box::new(BestExecution)),
        Some(other) => Err(format!(
            "unknown strategy '{other}' - expected one of: best_overall, best_price, best_liquidity, best_execution"
        )),
    }
}

/// Queries every venue backing `outcome_id`, builds `RoutingCandidate`s, and ranks them under
/// `strategy`. `venue_lookup` maps a venue_id to the adapter to query - the router doesn't own
/// adapters itself, `Aggregator` does.
pub async fn find_best_market(
    venue_refs: &[VenueMarketRef],
    size_usdc: &str,
    strategy: &dyn RoutingStrategy,
    venue_lookup: impl Fn(&str) -> Option<std::sync::Arc<dyn VenueAdapter>>,
    health: &HealthRegistry,
    cache: &crate::repositories::quote_cache::QuoteCache,
) -> Result<Recommendation, RoutingError> {
    if venue_refs.is_empty() {
        return Err(RoutingError::NoVenuesForOutcome("no venue markets provided".to_string()));
    }

    let requested_size: Decimal = size_usdc.parse().unwrap_or(Decimal::ZERO);

    // Quote every venue concurrently. These are independent network calls against different
    // hosts; running them in sequence put (venue count x per-request timeout) on a user-facing
    // request for no reason.
    let quoted = futures::future::join_all(venue_refs.iter().map(|venue_ref| {
        let adapter = venue_lookup(&venue_ref.venue_id);
        async move {
            let adapter = adapter?;
            // The adapter's own `.id()` is the canonical &'static str for this venue - used for
            // health tracking/lookups instead of `venue_ref.venue_id` (a plain String, since it
            // also has to round-trip through JSON on the canonical graph's API responses).
            let venue_id = adapter.id();

            // `venue_ref.venue_side` is this venue's own native side for the outcome being
            // requested ("yes"/"no" for Panta/Jupiter) - never a single side string reused across
            // venues, since the same canonical outcome maps to different native terms per venue.
            let side = venue_ref.venue_side.as_str();
            match crate::services::quoting::quote(&adapter, &venue_ref.venue_market_id, side, requested_size, health, cache).await {
                Ok(quote) => {
                    Some(RoutingCandidate {
                        venue_id,
                        venue_market_id: venue_ref.venue_market_id.clone(),
                        price: quote.avg_price,
                        executable_size: quote.effective_shares_or_size,
                        // The adapter already knows whether the whole amount filled - taking its
                        // answer instead of re-deriving one from rounded size x price.
                        fully_executable: quote.fully_filled,
                        fee_usdc: quote.fee_usdc,
                        routable: health.is_routable(venue_id),
                    })
                }
                // Health is recorded inside `quoting::quote`, by what the error means.
                Err(_) => None,
            }
        }
    }))
    .await;

    let candidates: Vec<RoutingCandidate> = quoted.into_iter().flatten().collect();

    if candidates.is_empty() {
        return Err(RoutingError::NoQuotesAvailable);
    }

    let ranked = strategy.rank(&candidates);
    let best = ranked.first().expect("candidates non-empty implies ranked non-empty");

    Ok(Recommendation {
        venue_id: best.candidate.venue_id,
        venue_market_id: best.candidate.venue_market_id.clone(),
        price: best.candidate.price,
        executable_size: best.candidate.executable_size,
        fees: best.candidate.fee_usdc,
        execution: if best.candidate.fully_executable { "available" } else { "partial" },
        comparison: ranked,
        // The caller knows the question; it sets this from the graph.
        rules_differ: false,
    })
}
