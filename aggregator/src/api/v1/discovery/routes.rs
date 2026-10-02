//! Venue health. The legacy `/events` and `/events/{id}/markets` routes that lived here were
//! removed with the title-similarity graph; `/v1/events` is the event API.

use axum::Json;
use axum::extract::State;

use crate::repositories::health_repository::VenueHealth;
use crate::state::AppState;

#[utoipa::path(
    get,
    path = "/venues/health",
    tag = "discovery",
    responses((status = 200, body = Vec<VenueHealth>, description = "Live reachability/failure-streak state for every registered venue."))
)]
/// Reports live health for every registered venue.
pub async fn venue_health(State(state): State<AppState>) -> Json<Vec<VenueHealth>> {
    Json(state.aggregator.health.snapshot())
}
