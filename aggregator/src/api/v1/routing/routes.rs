use axum::Json;
use axum::extract::State;
use rust_decimal::Decimal;

use crate::errors::ApiError;
use crate::models::routing::{FindBestMarketRequest, Recommendation};
use crate::services::routing::{find_best_market, strategy_by_name};
use crate::state::AppState;

#[utoipa::path(
    post,
    path = "/find-best-market",
    tag = "routing",
    request_body = FindBestMarketRequest,
    responses(
        (status = 200, body = Recommendation, description = "Best venue for this outcome/size under the chosen strategy, plus the full comparison set."),
        (status = 400, description = "size_usdc wasn't a positive number"),
        (status = 404, description = "No venue markets back this event/outcome"),
        (status = 502, description = "Every venue failed to quote"),
    )
)]
/// Routes a trade request to the best venue under the chosen strategy.
pub async fn find_best_market_route(
    State(state): State<AppState>,
    Json(req): Json<FindBestMarketRequest>,
) -> Result<Json<Recommendation>, ApiError> {
    match req.size_usdc.parse::<Decimal>() {
        Ok(size) if size > Decimal::ZERO => {}
        _ => return Err(ApiError::BadRequest(format!("size_usdc must be a positive number, got '{}'", req.size_usdc))),
    }

    let venue_refs = {
        let graph = state.aggregator.graph.read().await;
        graph.venue_markets_for_outcome(&req.event_id, &req.outcome_id)
    };

    if venue_refs.is_empty() {
        return Err(ApiError::NotFound(format!(
            "no venue markets for event '{}' outcome '{}'",
            req.event_id, req.outcome_id
        )));
    }

    let strategy = strategy_by_name(req.strategy.as_deref());

    let recommendation = find_best_market(
        &venue_refs,
        &req.size_usdc,
        strategy.as_ref(),
        |venue_id| state.aggregator.venues.iter().find(|v| v.id() == venue_id).cloned(),
        &state.aggregator.health,
    )
    .await?;

    Ok(Json(recommendation))
}
