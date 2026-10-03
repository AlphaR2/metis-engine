//! Central route registration - one nest per feature, mirroring each feature's OpenAPI tag.

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

use super::{catalog, discovery, execution, meta, positions};

/// Builds the full set of routes this service serves, unmounted (no state, no layers yet -
/// `app::run` attaches those).
pub fn configure() -> Router<AppState> {
    Router::new()
        .route("/v1/events", get(catalog::routes::list_events))
        .route("/v1/events/{id}", get(catalog::routes::get_event))
        .route("/v1/markets", get(catalog::routes::list_markets))
        .route("/v1/markets/{venue_id}/{venue_market_id}", get(catalog::routes::get_market))
        .route("/v1/markets/{venue_id}/{venue_market_id}/live", get(catalog::routes::get_market_live))
        .route("/v1/categories", get(catalog::routes::list_categories))
        .route("/v1/venues", get(catalog::routes::list_venues))
        .route("/v1/positions", get(positions::routes::list_positions))
        .route("/health", get(meta::routes::health))
        .route("/ready", get(meta::routes::ready))
        .route("/venues/health", get(discovery::routes::venue_health))
        .route("/execution/abilities", get(execution::routes::abilities))
        .route("/execution/orders", post(execution::routes::build_order))
        .route("/execution/markets", post(execution::routes::build_market))
        .route("/execution/claims", post(execution::routes::build_claim))
        .route("/execution/sells", post(execution::routes::build_sell))
        .route("/execution/confirm", post(execution::routes::confirm))
        .route("/execution/{venue_id}/{venue_ref}", get(execution::routes::status))
}

/// Operator routes: metrics and a manual refresh. Open in the MVP.
pub fn configure_admin() -> Router<AppState> {
    Router::new()
        .route("/metrics", get(meta::routes::metrics_route))
        .route("/refresh", post(meta::routes::refresh))
}
