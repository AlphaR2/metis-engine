//! Pure scoring math for ranking venue candidates. No I/O, no health/graph access - just
//! `RoutingCandidate` in, `ScoredCandidate`s out. `services::routing::service` owns the
//! `RoutingStrategy` trait/orchestration and calls into these.

use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;

use crate::models::routing::{RoutingCandidate, ScoredCandidate};

/// How far below its raw score an unroutable candidate gets pushed - large enough that no
/// realistic price/liquidity value could out-rank a routable one, small enough the candidate
/// still sorts sanely relative to other unroutable ones.
fn unroutable_penalty() -> Decimal {
    Decimal::from(1_000_000)
}

/// Shared scoring scaffold every strategy uses: apply `score_fn`, then sort,
/// after penalizing unroutable candidates so they never rank first.
fn routable_first_then<F: Fn(&RoutingCandidate) -> Decimal>(
    candidates: &[RoutingCandidate],
    score_fn: F,
) -> Vec<ScoredCandidate> {
    let mut scored: Vec<ScoredCandidate> = candidates
        .iter()
        .map(|c| {
            // Unroutable candidates are pushed to the bottom via a large negative offset, not
            // filtered out entirely - the UI still gets to see (and explain) a good price that
            // isn't currently safe to route to.
            let base = score_fn(c);
            let score = if c.routable { base } else { base - unroutable_penalty() };
            ScoredCandidate { candidate: c.clone(), score }
        })
        .collect();
    // Decimal has a real total order (unlike f64/NaN), so a plain `cmp` is enough - no
    // partial_cmp fallback needed.
    scored.sort_by_key(|a| std::cmp::Reverse(a.score));
    scored
}

/// Ranks purely by lowest price - lower is better for a buyer, so score is negated price.
pub fn score_best_price(candidates: &[RoutingCandidate]) -> Vec<ScoredCandidate> {
    routable_first_then(candidates, |c| -c.price)
}

/// Ranks by how much size a venue can actually fill.
pub fn score_best_liquidity(candidates: &[RoutingCandidate]) -> Vec<ScoredCandidate> {
    routable_first_then(candidates, |c| c.executable_size)
}

/// Ranks by whether the full requested size can be filled at all; price breaks ties among those
/// that can.
pub fn score_best_execution(candidates: &[RoutingCandidate]) -> Vec<ScoredCandidate> {
    routable_first_then(candidates, |c| {
        if c.fully_executable { -c.price } else { -Decimal::from(1_000) - c.price }
    })
}

/// The default: a weighted blend of price, liquidity, and fillability. Not tuned against real
/// trading data yet - deliberately simple until there's a second real use case demanding a
/// different weighting.
pub fn score_best_overall(candidates: &[RoutingCandidate]) -> Vec<ScoredCandidate> {
    routable_first_then(candidates, |c| {
        let price_component = -c.price * Decimal::from(10);
        // ln(1 + executable_size), computed manually since Decimal has no ln_1p - checked_ln
        // avoids a panic on a pathological non-positive executable_size.
        let liquidity_component = (Decimal::ONE + c.executable_size)
            .checked_ln()
            .unwrap_or(Decimal::ZERO)
            * Decimal::new(1, 1); // * 0.1
        let executable_bonus = if c.fully_executable { Decimal::ONE } else { Decimal::ZERO };
        price_component + liquidity_component + executable_bonus
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(venue_id: &'static str, price: &str, size: &str, routable: bool) -> RoutingCandidate {
        RoutingCandidate {
            venue_id,
            venue_market_id: "m".to_string(),
            price: price.parse().unwrap(),
            executable_size: size.parse().unwrap(),
            fully_executable: true,
            fee_usdc: Decimal::ZERO,
            routable,
        }
    }

    #[test]
    fn best_price_prefers_lower_price() {
        let candidates = vec![candidate("a", "0.57", "100", true), candidate("b", "0.55", "100", true)];
        let ranked = score_best_price(&candidates);
        assert_eq!(ranked[0].candidate.venue_id, "b");
    }

    #[test]
    fn best_liquidity_prefers_more_executable_size() {
        let candidates = vec![candidate("a", "0.5", "40000", true), candidate("b", "0.5", "5000", true)];
        let ranked = score_best_liquidity(&candidates);
        assert_eq!(ranked[0].candidate.venue_id, "a");
    }

    #[test]
    fn unroutable_candidate_never_ranks_first_even_with_the_best_price() {
        let candidates =
            vec![candidate("cheap_but_down", "0.01", "100", false), candidate("normal", "0.5", "100", true)];
        let ranked = score_best_price(&candidates);
        assert_eq!(ranked[0].candidate.venue_id, "normal");
    }

    #[test]
    fn best_execution_only_credits_full_fills() {
        let mut partial = candidate("thin", "0.4", "10", true);
        partial.fully_executable = false;
        let full = candidate("deep", "0.6", "100", true);
        let ranked = score_best_execution(&[partial, full]);
        assert_eq!(ranked[0].candidate.venue_id, "deep");
    }
}
