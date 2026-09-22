//! Server construction and wiring - builds every dependency, assembles the router, and serves.
//! `main.rs` is process bootstrap only; this is where the actual service lives.

use std::sync::Arc;

use axum::Router;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::aggregator::Aggregator;
use crate::api::v1;
use crate::config::Config;
use crate::database;
use crate::middleware::{cors_layer, require_admin_token};
use crate::openapi::ApiDoc;
use crate::services::VenueAdapter;
use crate::services::jupiter::JupiterAdapter;
use crate::services::panta::PantaAdapter;
use crate::services::pascal::PascalAdapter;
use crate::state::AppState;

/// Builds every dependency, wires the router, and serves forever.
pub async fn run(config: Config) {
    if config.panta_api_key.is_empty() {
        tracing::warn!("PANTA_API_KEY not set - Panta adapter will fail every call");
    }
    if config.jupiter_api_key.is_empty() {
        tracing::warn!("JUPITER_API_KEY not set - Jupiter adapter will fail every call");
    }

    let mut venues: Vec<Arc<dyn VenueAdapter>> = Vec::new();
    if config.enabled_venues.iter().any(|v| v == "panta") {
        venues.push(Arc::new(PantaAdapter::new(
            config.panta_base_url.clone(),
            config.panta_api_key.clone(),
            config.panta_quote_wallet.clone(),
        )));
    }
    if config.enabled_venues.iter().any(|v| v == "jupiter") {
        venues.push(Arc::new(JupiterAdapter::new(config.jupiter_base_url.clone(), config.jupiter_api_key.clone())));
    }
    if config.enabled_venues.iter().any(|v| v == "pascal") {
        tracing::warn!("pascal enabled - its wire schema is unverified (its /markets envelope is {{status, data}}) and it reports no tradeability flag");
        venues.push(Arc::new(PascalAdapter::new(config.pascal_base_url.clone())));
    }
    tracing::info!("enabled venues: {:?}", config.enabled_venues);


    // Persistence is optional: unset or unreachable `DATABASE_URL` logs a warning and the server
    // runs in-memory-only rather than refusing to boot. The market store is the read path either
    // way - Postgres only adds durability across restarts.
    let db = match &config.database_url {
        Some(url) => match database::connect(url, config.database_pool_size).await {
            Ok(pool) => {
                tracing::info!("connected to Postgres (pool size {})", config.database_pool_size);
                crate::metrics::DB_POOL_SIZE.set(config.database_pool_size as i64);
                Some(pool)
            }
            Err(e) => {
                tracing::warn!("could not connect to DATABASE_URL: {e} - running in-memory-only");
                None
            }
        },
        None => {
            tracing::warn!("DATABASE_URL not set - running in-memory-only, nothing persists across restarts");
            None
        }
    };

    let aggregator = Arc::new(Aggregator::new(venues, db.clone()));

    // Serve the previous catalog straight away rather than an empty list while the first refresh
    // runs - a restart shouldn't look like an outage to the UI.
    let restored = aggregator.warm_start().await;
    if restored > 0 {
        tracing::info!("warm start: restored {restored} markets from Postgres");
    }

    // Prime the catalog at boot, then keep it fresh on a timer - but don't block the server on
    // either. A full refresh across every venue and category takes a while, and there's no reason
    // a slow venue should delay health/swagger from coming up. `/markets` reports
    // `last_refreshed_at` so a caller can tell an empty catalog from an unprimed one.
    let background_aggregator = aggregator.clone();
    let refresh_interval = config.refresh_interval_secs;
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(refresh_interval));
        // The first tick fires immediately, which is exactly the boot-time prime.
        loop {
            ticker.tick().await;
            background_aggregator.refresh().await;
        }
    });
    tracing::info!("background refresh every {refresh_interval}s");

    let state = AppState {
        aggregator,
        db,
        admin_token: config.admin_token.clone().map(Arc::new),
        execution_enabled: config.execution_enabled,
        market_creation_enabled: config.market_creation_enabled,
    };

    // Both are off unless deliberately switched on; say so at boot so the state of a deployment is
    // never a surprise discovered through a refused request.
    tracing::info!(
        "execution: trading={} market_creation={}",
        if config.execution_enabled { "ENABLED" } else { "disabled" },
        if config.market_creation_enabled { "ENABLED" } else { "disabled" },
    );

    if config.admin_token.is_none() {
        tracing::warn!("ADMIN_TOKEN not set - POST /refresh and GET /metrics will refuse every request");
    }

    // Operator routes are gated; everything else is public. Split into two routers so the auth
    // layer applies to exactly those two and nothing else.
    let admin_routes = v1::route::configure_admin()
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), require_admin_token));

    let app = Router::new()
        .merge(v1::route::configure())
        .merge(admin_routes)
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .layer(cors_layer(&config.cors_origin))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", config.port))
        .await
        .unwrap_or_else(|e| panic!("failed to bind port {}: {e}", config.port));

    tracing::info!("metis-api listening on :{} - swagger UI at /swagger-ui", config.port);
    axum::serve(listener, app).await.expect("server error");
}
