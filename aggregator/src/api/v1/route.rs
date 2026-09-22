//! Central route registration - one nest per feature, mirroring each feature's OpenAPI tag.

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

use super::{discovery, execution, markets, meta, positions, raw, routing};

/// Builds the full set of routes this service serves, unmounted (no state, no layers yet -
/// `app::run` attaches those).
pub fn configure() -> Router<AppState> {
    Router::new()
        .route("/health", get(meta::routes::health))
        .route("/markets", get(markets::routes::list_markets))
        .route("/categories", get(markets::routes::list_categories))
        .route("/events", get(discovery::routes::list_events))
        .route("/events/{id}/markets", get(discovery::routes::get_event_markets))
        .route("/venues/health", get(discovery::routes::venue_health))
        .route("/venues/{venue_id}/raw/{*path}", get(raw::routes::raw_get))
        .route("/find-best-market", post(routing::routes::find_best_market_route))
        .route("/execution/abilities", get(execution::routes::abilities))
        .route("/execution/orders", post(execution::routes::build_order))
        .route("/execution/markets", post(execution::routes::build_market))
        .route("/execution/claims", post(execution::routes::build_claim))
        .route("/execution/confirm", post(execution::routes::confirm))
        .route("/execution/{venue_id}/{venue_ref}", get(execution::routes::status))
        .route(
            "/positions",
            post(positions::routes::record_position).get(positions::routes::list_positions),
        )
}

/// Operator-only routes, mounted behind the admin-token gate by `app::run`. Kept separate from
/// `configure()` so the auth layer can't accidentally cover (or miss) the wrong endpoints.
pub fn configure_admin() -> Router<AppState> {
    Router::new()
        .route("/metrics", get(meta::routes::metrics_route))
        .route("/refresh", post(meta::routes::refresh))
}
