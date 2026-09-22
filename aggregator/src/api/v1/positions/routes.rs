use axum::Json;
use axum::extract::{Query, State};

use crate::models::position::{ListPositionsQuery, Position, RecordPositionRequest};
use crate::state::AppState;

#[utoipa::path(
    post,
    path = "/positions",
    tag = "positions",
    request_body = RecordPositionRequest,
    responses((status = 200, body = Position, description = "Records a position that already happened out-of-band (no execution here)."))
)]
/// Records a position that happened out-of-band, no execution here.
pub async fn record_position(
    State(state): State<AppState>,
    Json(req): Json<RecordPositionRequest>,
) -> Json<Position> {
    Json(state.aggregator.positions.record(req))
}

#[utoipa::path(
    get,
    path = "/positions",
    tag = "positions",
    params(ListPositionsQuery),
    responses((status = 200, body = Vec<Position>, description = "Every canonical position recorded for this wallet, across every venue."))
)]
/// Lists every position recorded for a given wallet.
pub async fn list_positions(
    State(state): State<AppState>,
    Query(query): Query<ListPositionsQuery>,
) -> Json<Vec<Position>> {
    Json(state.aggregator.positions.for_wallet(&query.wallet))
}
