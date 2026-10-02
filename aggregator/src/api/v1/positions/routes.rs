//! `GET /v1/positions` - what a wallet holds, across every venue that can say.
//!
//! Read-only and public: a wallet's holdings are already public on-chain, so there is nothing to
//! authenticate, and nothing here can move funds. (The earlier `/positions` was removed because it
//! *wrote* unauthenticated state, audit A10; this only reads.) Each call costs venue requests, so it
//! is rate-limited like a live quote, and complete results are cached for a few seconds.
//!
//! The wallet is never logged.

use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::errors::{ApiError, ErrorCode};
use crate::models::position::PositionsResponse;
use crate::services::positions::read_positions;
use crate::state::AppState;
use crate::utils::solana_tx::decode_32;

#[derive(Debug, Deserialize, IntoParams)]
pub struct PositionsQuery {
    /// The wallet's base58 public key.
    pub wallet: String,
}

#[utoipa::path(
    get,
    path = "/v1/positions",
    tag = "v1",
    params(PositionsQuery),
    responses(
        (status = 200, body = PositionsResponse, description = "The wallet's positions. When `venues_failed` is non-empty the list is incomplete."),
        (status = 400, description = "The wallet is not a valid base58 public key")
    )
)]
/// A wallet's positions across venues, with claim references and sale support where they apply.
///
/// **Check `venues_failed`.** A venue that could not be read is listed there instead of being
/// reported as holding nothing, so an empty `positions` with a non-empty `venues_failed` means "could
/// not load", not "no positions".
pub async fn list_positions(
    State(state): State<AppState>,
    Query(query): Query<PositionsQuery>,
) -> Result<Json<PositionsResponse>, ApiError> {
    let wallet = query.wallet.trim();
    decode_32(wallet)
        .map_err(|_| ApiError::bad_request(ErrorCode::InvalidRequest, "wallet must be a base58 Solana public key"))?;
    let graph = state.aggregator.events.read().await.clone();
    Ok(Json(read_positions(&state.aggregator, &graph, wallet).await))
}
