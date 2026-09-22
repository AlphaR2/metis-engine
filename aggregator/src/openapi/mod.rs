//! Root OpenAPI document, served live at `/api-docs/openapi.json` with a Swagger UI at
//! `/swagger-ui`.

use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Metis Aggregator API",
        description = "Discovers and routes across a curated network of prediction-market venues. `GET /markets` is the primary path: raw venue markets by category, filtered to those whose betting closes within a window, ranked by price/liquidity/execution. The canonical event graph under `/events` is retained for the multi-venue case. Read-only today; execution is a later phase.",
        version = "0.1.0"
    ),
    paths(
        crate::api::v1::meta::routes::health,
        crate::api::v1::meta::routes::refresh,
        crate::api::v1::meta::routes::metrics_route,
        crate::api::v1::execution::routes::abilities,
        crate::api::v1::execution::routes::build_order,
        crate::api::v1::execution::routes::build_market,
        crate::api::v1::execution::routes::build_claim,
        crate::api::v1::execution::routes::confirm,
        crate::api::v1::execution::routes::status,
        crate::api::v1::markets::routes::list_markets,
        crate::api::v1::markets::routes::list_categories,
        crate::api::v1::discovery::routes::list_events,
        crate::api::v1::discovery::routes::get_event_markets,
        crate::api::v1::discovery::routes::venue_health,
        crate::api::v1::raw::routes::raw_get,
        crate::api::v1::routing::routes::find_best_market_route,
        crate::api::v1::positions::routes::record_position,
        crate::api::v1::positions::routes::list_positions,
    ),
    components(schemas(
        crate::api::v1::execution::routes::BuildResponse,
        crate::api::v1::execution::routes::ConfirmRequest,
        crate::api::v1::execution::routes::AbilitiesResponse,
        crate::api::v1::execution::routes::VenueAbilities,
        crate::models::execution::UnsignedTransaction,
        crate::models::execution::TransactionCost,
        crate::models::execution::OrderPreview,
        crate::models::execution::CostLine,
        crate::models::execution::IntentKind,
        crate::models::execution::ActionStatus,
        crate::models::execution::ConfirmOutcome,
        crate::models::execution::BuyIntent,
        crate::models::execution::CreateMarketIntent,
        crate::models::execution::ClaimIntent,
        crate::api::v1::raw::routes::RawResponse,
        crate::api::v1::raw::routes::FieldCoverage,
        crate::api::v1::markets::routes::ListMarketsResponse,
        crate::api::v1::markets::routes::MarketEntry,
        crate::api::v1::markets::routes::CategoriesResponse,
        crate::models::venue::VenueMarket,
        crate::models::event::Event,
        crate::models::event::Proposition,
        crate::models::event::Outcome,
        crate::models::event::VenueMarketRef,
        crate::models::event::EventSummary,
        crate::models::event::EventTradeStatus,
        crate::api::v1::meta::routes::HealthResponse,
        crate::models::routing::FindBestMarketRequest,
        crate::models::routing::Recommendation,
        crate::models::routing::RoutingCandidate,
        crate::models::routing::ScoredCandidate,
        crate::models::position::Position,
        crate::models::position::PositionStatus,
        crate::models::position::RecordPositionRequest,
        crate::repositories::health_repository::VenueHealth,
    )),
    tags(
        (name = "execution", description = "Building transactions for a wallet to sign - trading, market creation, claims"),
        (name = "raw", description = "Untouched venue responses, for seeing what a venue actually sends"),
        (name = "markets", description = "Raw venue markets by category - the primary discovery path"),
        (name = "meta", description = "Liveness and manual refresh"),
        (name = "discovery", description = "Canonical events, venue markets, venue health"),
        (name = "routing", description = "The find_best_market recommendation primitive"),
        (name = "positions", description = "Canonical cross-venue position index"),
    )
)]
/// The generated OpenAPI document, served at /api-docs/openapi.json.
pub struct ApiDoc;
