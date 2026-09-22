//! Market listing: catalog ranking by default, real quotes on request.
//!
//! The cheap path sorts what the refresh already pulled (`maths::markets`) and costs nothing but
//! an in-memory read. When a caller supplies a trade size, the shortlist that survives the cheap
//! sort is re-priced against the venues for real and re-ranked by `maths::routing` - the same
//! scorers `/find-best-market` uses, so a market's rank here and its rank as a routing candidate
//! can't drift apart.

use std::sync::Arc;

use rust_decimal::Decimal;

use crate::models::routing::{RoutingCandidate, ScoredCandidate};
use crate::models::venue::VenueMarket;
use crate::repositories::health_repository::HealthRegistry;
use crate::repositories::market_repository::MarketSort;
use crate::services::VenueAdapter;
use crate::services::maths::routing as routing_maths;

/// How many markets get live-quoted when `quote_size` is supplied. Every one is an outbound
/// request, so this is capped well below a typical page: the cheap sort has already put the
/// plausible candidates on top, and re-pricing the tail would cost real latency to reorder
/// markets nobody was going to reach.
pub const MAX_LIVE_QUOTES: usize = 20;

/// How the returned markets were ranked - surfaced to the caller so a UI can say whether it's
/// showing catalog estimates or real quotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoredWith {
    Catalog,
    LiveQuotes,
}

impl ScoredWith {
    pub fn as_str(self) -> &'static str {
        match self {
            ScoredWith::Catalog => "catalog",
            ScoredWith::LiveQuotes => "live_quotes",
        }
    }
}

/// Quotes every market in `shortlist` concurrently and ranks the results under `sort`.
///
/// Concurrency is the point: these are independent network calls, and doing them in sequence
/// would put `MAX_LIVE_QUOTES` × the per-request timeout on a user-facing request.
///
/// Markets that fail to quote are dropped rather than floated on a stale catalog price - the
/// caller asked what this size actually costs, and "we don't know" isn't something to rank. A
/// market with no depth at this size is a *business* answer, not a venue fault, so it never counts
/// against venue health; that distinction is what `AdapterError::NoLiquidity` exists for.
///
/// Returns each surviving market paired with its score, best-first.
pub async fn rank_with_quotes<'a>(
    shortlist: &[&'a VenueMarket],
    size_usdc: Decimal,
    side: &str,
    sort: MarketSort,
    venue_lookup: impl Fn(&str) -> Option<Arc<dyn VenueAdapter>>,
    health: &HealthRegistry,
) -> Vec<(&'a VenueMarket, ScoredCandidate)> {
    let quoted = futures::future::join_all(shortlist.iter().map(|market| {
        let adapter = venue_lookup(&market.venue_id);
        async move {
            let Some(adapter) = adapter else {
                tracing::warn!("no adapter registered for venue '{}' - is it in ENABLED_VENUES?", market.venue_id);
                return None;
            };
            let venue_id = adapter.id();
            match adapter.quote(&market.venue_market_id, side, size_usdc).await {
                Ok(quote) => {
                    health.record_success(venue_id);
                    Some((
                        *market,
                        RoutingCandidate {
                            venue_id,
                            venue_market_id: market.venue_market_id.clone(),
                            price: quote.avg_price,
                            executable_size: quote.effective_shares_or_size,
                            fully_executable: quote.fully_filled,
                            fee_usdc: quote.fee_usdc,
                            routable: health.is_routable(venue_id),
                        },
                    ))
                }
                Err(err) => {
                    // Log before dropping. A market silently vanishing from a ranked list is the
                    // kind of thing that looks like "no results" rather than "every quote failed".
                    if err.is_business_outcome() {
                        tracing::debug!("{venue_id}: no quote for {}: {err}", market.venue_market_id);
                    } else {
                        tracing::warn!("{venue_id}: quote failed for {}: {err}", market.venue_market_id);
                        health.record_failure(venue_id, err.to_string());
                    }
                    None
                }
            }
        }
    }))
    .await;

    let pairs: Vec<(&VenueMarket, RoutingCandidate)> = quoted.into_iter().flatten().collect();
    if pairs.is_empty() {
        return Vec::new();
    }

    let candidates: Vec<RoutingCandidate> = pairs.iter().map(|(_, c)| c.clone()).collect();
    let ranked = match sort {
        MarketSort::BestPrice => routing_maths::score_best_price(&candidates),
        MarketSort::BestLiquidity => routing_maths::score_best_liquidity(&candidates),
        // A caller who asked for closing-soonest but supplied a size still wants the time
        // ordering; scoring is only used to attach real prices, so rank on the blend and let the
        // route restore chronological order.
        MarketSort::BestOverall | MarketSort::ClosingSoonest => routing_maths::score_best_overall(&candidates),
    };

    // Scoring reorders the candidates, so pair each back to its market by identity, not position.
    ranked
        .into_iter()
        .filter_map(|scored| {
            pairs
                .iter()
                .find(|(_, c)| {
                    c.venue_market_id == scored.candidate.venue_market_id && c.venue_id == scored.candidate.venue_id
                })
                .map(|(market, _)| (*market, scored))
        })
        .collect()
}
