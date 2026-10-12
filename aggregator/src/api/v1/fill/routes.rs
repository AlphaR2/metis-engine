//! The Fill Score route: the same call on every venue that lists it, ranked by how likely it is to fill.

use std::time::Duration;

use axum::Json;
use axum::extract::{Query, State};
use rust_decimal::Decimal;
use serde::Deserialize;
use utoipa::IntoParams;

use crate::errors::ApiError;
use crate::services::fill::{FillScore, Listing, METHOD, rank, score};
use crate::state::AppState;

/// A venue that does not answer in this long is scored as having no quote, so one slow venue cannot hold the call.
const QUOTE_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Deserialize, IntoParams)]
pub struct FillScoreQuery {
    /// The market the call is on.
    pub venue_id: String,
    pub venue_market_id: String,
    /// `yes` or `no` (default `yes`).
    pub side: Option<String>,
    /// How much USDC the bet is for (default 10).
    pub amount_usdc: Option<String>,
}

#[utoipa::path(
    get,
    path = "/v1/fill-score",
    tag = "v1",
    params(FillScoreQuery),
    responses(
        (status = 200, body = FillScore, description = "The market and every listing of the same question, scored and ranked best first."),
        (status = 400, description = "Bad side or amount"),
        (status = 404, description = "No such market in the catalog"),
    )
)]
/// Scores the same call across the venues that list it.
pub async fn fill_score(State(state): State<AppState>, Query(query): Query<FillScoreQuery>) -> Result<Json<FillScore>, ApiError> {
    let side = match query.side.as_deref() {
        None | Some("yes") => "yes",
        Some("no") => "no",
        Some(other) => return Err(ApiError::BadRequest(format!("unknown side '{other}' - expected 'yes' or 'no'"))),
    };
    let amount = match query.amount_usdc.as_deref() {
        None => Decimal::from(10),
        Some(raw) => match raw.parse::<Decimal>() {
            Ok(value) if value > Decimal::ZERO => value,
            _ => return Err(ApiError::BadRequest(format!("amount_usdc must be a positive number, got '{raw}'"))),
        },
    };

    // The market itself, then every listing of the same question the graph found.
    let listings: Vec<(String, String, String, bool, Option<Decimal>)> = {
        let graph = state.aggregator.events.read().await.clone();
        let (_, node) = graph
            .find_market(&query.venue_id, &query.venue_market_id)
            .ok_or_else(|| ApiError::NotFound(format!("no market '{}' on '{}'", query.venue_market_id, query.venue_id)))?;
        let mut found = vec![node.market.clone()];
        for other in &node.same_question_as {
            if let Some((_, matched)) = graph.find_market(&other.venue_id, &other.venue_market_id) {
                found.push(matched.market.clone());
            }
        }
        found
            .into_iter()
            .map(|m| (m.venue_id.clone(), m.venue_market_id.clone(), m.title.clone(), !(m.display_only || m.dead), m.liquidity))
            .collect()
    };

    let quoted = futures::future::join_all(listings.into_iter().map(|(venue_id, venue_market_id, title, tradable, depth)| {
        let state = state.clone();
        async move {
            let routable = state.aggregator.health.is_routable(&venue_id);
            let quote = if !tradable {
                Err("not tradable".to_string())
            } else if !routable {
                Err("the venue is failing right now".to_string())
            } else {
                match state.aggregator.venue(&venue_id) {
                    None => Err("the venue is not enabled".to_string()),
                    Some(adapter) => match tokio::time::timeout(QUOTE_TIMEOUT, adapter.quote(&venue_market_id, side, amount)).await {
                        Ok(Ok(quote)) => Ok(quote),
                        Ok(Err(error)) => Err(error.to_string()),
                        Err(_) => Err("the venue did not answer in time".to_string()),
                    },
                }
            };
            score(amount, Listing { venue_id, venue_market_id, title, tradable, depth, quote })
        }
    }))
    .await;

    let mut options = quoted;
    rank(&mut options);
    Ok(Json(FillScore { side: side.to_string(), amount_usdc: amount, method: METHOD.to_string(), options }))
}
