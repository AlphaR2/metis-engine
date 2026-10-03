//! Reading a wallet's positions across every venue that can report them.
//!
//! The rule that shapes this module: **a venue that could not be read is not a venue where the
//! wallet holds nothing.** Each venue is read on its own with its own timeout, a failure is
//! reported in `venues_failed`, and the list that comes back says plainly that it is incomplete.
//! Turning a failure into an empty list would show a user with open positions an empty portfolio.

use std::time::Duration;

use rust_decimal::Decimal;

use crate::aggregator::Aggregator;
use crate::engine::graph::{EventGraph, MarketNode};
use crate::errors::{AdapterError, ApiError};
use crate::models::position::{Position, PositionState, PositionsResponse, VenueFailure};
use crate::utils::time::now_unix;

/// How long one venue gets before it is reported as failed. Shorter than the request timeout, so a
/// slow venue costs the caller its own rows, not the whole response.
const VENUE_TIMEOUT: Duration = Duration::from_secs(20);

/// The price the catalog holds for one side of a market.
fn catalog_price(node: &MarketNode, side: &str) -> Option<Decimal> {
    let price = match side.to_ascii_lowercase().as_str() {
        "no" | "short" => node.market.no_price.as_deref(),
        _ => node.market.yes_price.as_deref(),
    };
    price.and_then(|p| p.parse().ok())
}

/// Adds what the event graph knows: the event, the market's own title if the venue gave none, and,
/// for an open position the venue did not value, an **estimate** from the catalog price (flagged
/// as one).
fn enrich(position: &mut Position, graph: &EventGraph) {
    let Some((event, node)) = graph.find_market(&position.venue_id, &position.venue_market_id) else {
        return;
    };
    position.event_id = Some(event.id.clone());
    position.event_title = Some(event.title.clone());
    if position.title.is_none() {
        position.title = Some(node.market.title.clone());
    }
    if position.value_usdc.is_none()
        && position.state == PositionState::Open
        && position.contracts > Decimal::ZERO
        && let Some(price) = catalog_price(node, &position.side)
    {
        position.value_usdc = Some(position.contracts * price);
        position.value_is_estimate = true;
    }
}

/// Claimable first (money waiting), then open, then finished. Stable within a group.
fn sort_positions(positions: &mut [Position]) {
    let rank = |state: PositionState| match state {
        PositionState::Claimable => 0,
        PositionState::Open => 1,
        PositionState::Claimed => 2,
        PositionState::Lost => 3,
        PositionState::Cancelled => 4,
    };
    positions.sort_by(|a, b| {
        rank(a.state)
            .cmp(&rank(b.state))
            .then_with(|| a.venue_id.cmp(&b.venue_id))
            .then_with(|| a.venue_market_id.cmp(&b.venue_market_id))
            .then_with(|| a.side.cmp(&b.side))
    });
}

/// Folds each venue's outcome into one response. A venue that says it does not list positions is
/// simply left out; any other failure is recorded, and the response is then incomplete.
fn assemble(wallet: &str, results: Vec<(String, Result<Vec<Position>, AdapterError>)>, graph: &EventGraph, now: i64) -> PositionsResponse {
    let mut positions = Vec::new();
    let mut venues_ok = Vec::new();
    let mut venues_failed = Vec::new();
    for (venue_id, result) in results {
        match result {
            Ok(rows) => {
                venues_ok.push(venue_id);
                positions.extend(rows);
            }
            Err(AdapterError::Unsupported { .. }) => {}
            Err(error) => {
                let (code, message) = ApiError::from(error).parts();
                venues_failed.push(VenueFailure { venue_id, code, message });
            }
        }
    }
    for position in &mut positions {
        enrich(position, graph);
    }
    sort_positions(&mut positions);
    PositionsResponse { wallet: wallet.to_string(), positions, venues_failed, venues_ok, fetched_at: now }
}

/// A wallet's positions on every venue that can report them, read concurrently.
///
/// A complete result is cached for a few seconds; an incomplete one never is.
pub async fn read_positions(aggregator: &Aggregator, graph: &EventGraph, wallet: &str) -> PositionsResponse {
    if let Some(cached) = aggregator.positions.get(wallet) {
        return cached;
    }

    let reads = aggregator.venues.iter().filter(|v| v.capabilities().can_list_positions).map(|venue| {
        let venue = venue.clone();
        async move {
            let id = venue.id().to_string();
            let result = match tokio::time::timeout(VENUE_TIMEOUT, venue.positions(wallet)).await {
                Ok(result) => result,
                Err(_) => Err(AdapterError::Venue(format!("{id} did not answer within {}s", VENUE_TIMEOUT.as_secs()))),
            };
            (id, result)
        }
    });
    let results = futures::future::join_all(reads).await;

    let response = assemble(wallet, results, graph, now_unix());
    aggregator.positions.insert(&response);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorCode;

    fn position(venue: &str, market: &str, side: &str, state: PositionState) -> Position {
        Position {
            venue_id: venue.to_string(),
            venue_market_id: market.to_string(),
            venue_position_id: None,
            side: side.to_string(),
            contracts: Decimal::from(10),
            avg_price_usdc: None,
            cost_usdc: None,
            value_usdc: None,
            value_is_estimate: false,
            pnl_usdc: None,
            payout_usdc: None,
            state,
            phase: None,
            open_orders: 0,
            claim_ref: None,
            can_sell: false,
            title: None,
            event_id: None,
            event_title: None,
            updated_at: None,
        }
    }

    fn graph() -> EventGraph {
        EventGraph::default()
    }

    #[test]
    fn a_failed_venue_is_reported_and_never_shown_as_empty() {
        let results = vec![
            ("panta".to_string(), Ok(vec![position("panta", "M1", "yes", PositionState::Open)])),
            ("jupiter".to_string(), Err(AdapterError::Venue("jupiter positions: HTTP 503 - down".into()))),
        ];
        let response = assemble("W", results, &graph(), 1);
        assert_eq!(response.positions.len(), 1);
        assert_eq!(response.venues_ok, vec!["panta"]);
        assert_eq!(response.venues_failed.len(), 1, "the list is incomplete and says so");
        assert_eq!(response.venues_failed[0].venue_id, "jupiter");
        assert_eq!(response.venues_failed[0].code, ErrorCode::VenueUnavailable);
    }

    #[test]
    fn a_venue_that_reads_fine_and_holds_nothing_is_ok_not_failed() {
        let response = assemble("W", vec![("panta".to_string(), Ok(vec![]))], &graph(), 1);
        assert!(response.positions.is_empty() && response.venues_failed.is_empty());
        assert_eq!(response.venues_ok, vec!["panta"]);
    }

    #[test]
    fn a_venue_without_position_support_is_left_out_not_failed() {
        let unsupported = AdapterError::Unsupported { venue: "x", action: "listing positions" };
        let response = assemble("W", vec![("x".to_string(), Err(unsupported))], &graph(), 1);
        assert!(response.venues_failed.is_empty() && response.venues_ok.is_empty());
    }

    #[test]
    fn every_venue_failing_is_a_response_of_failures_not_an_empty_portfolio() {
        let results = vec![
            ("panta".to_string(), Err(AdapterError::Venue("down".into()))),
            ("jupiter".to_string(), Err(AdapterError::Venue("down".into()))),
        ];
        let response = assemble("W", results, &graph(), 1);
        assert!(response.positions.is_empty());
        assert_eq!(response.venues_failed.len(), 2);
        assert!(response.venues_ok.is_empty());
    }

    #[test]
    fn claimable_comes_first_then_open_then_finished() {
        let mut positions = vec![
            position("jupiter", "A", "yes", PositionState::Lost),
            position("panta", "B", "yes", PositionState::Open),
            position("jupiter", "C", "no", PositionState::Claimable),
            position("panta", "D", "yes", PositionState::Claimed),
        ];
        sort_positions(&mut positions);
        let order: Vec<&str> = positions.iter().map(|p| p.venue_market_id.as_str()).collect();
        assert_eq!(order, ["C", "B", "D", "A"]);
    }

    #[test]
    fn a_venue_timeout_is_a_failure_with_a_message() {
        let results = vec![("jupiter".to_string(), Err(AdapterError::Venue("jupiter did not answer within 20s".into())))];
        let response = assemble("W", results, &graph(), 1);
        assert!(response.venues_failed[0].message.contains("did not answer"));
    }
}
