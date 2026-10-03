use std::sync::Arc;

use sqlx::PgPool;

use crate::aggregator::Aggregator;

/// Shared axum state - one aggregator instance for every request.
#[derive(Clone)]
pub struct AppState {
    pub aggregator: Arc<Aggregator>,
    /// `None` when `DATABASE_URL` is unset or unreachable at boot - the server still runs
    /// in-memory-only in that case rather than refusing to start. Not yet read by any
    /// repository; wired in ahead of persistence landing so `/health` can report DB reachability
    /// as soon as a pool exists.
    pub db: Option<PgPool>,
    /// Whether this deployment will build buy transactions. Off by default - see `Config`.
    pub execution_enabled: bool,
    /// Whether this deployment will build market-creation transactions. Off by default.
    pub market_creation_enabled: bool,
    /// Per-client limiter for the public routes.
    pub rate_limiter: Arc<crate::middleware::RateLimiter>,
}
