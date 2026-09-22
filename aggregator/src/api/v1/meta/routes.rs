use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use utoipa::ToSchema;

use crate::state::AppState;

/// Response body for `GET /health`.
#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    /// "ok" or "degraded" - degraded means the DB pool is configured but unreachable, not that
    /// the server can't serve requests (it still can, in-memory-only).
    pub status: &'static str,
    /// `true` if `DATABASE_URL` is set at all, regardless of reachability.
    pub database_configured: bool,
    /// `true` if a DB ping just succeeded - always `false` when `database_configured` is `false`.
    pub database_reachable: bool,
}

/// Liveness check - reports DB pool reachability alongside the process being up, so a caller can
/// tell "running in-memory-only" apart from "fully healthy" without a separate endpoint.
#[utoipa::path(get, path = "/health", tag = "meta", responses((status = 200, body = HealthResponse), (status = 503, body = HealthResponse)))]
pub async fn health(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    match &state.db {
        None => (StatusCode::OK, Json(HealthResponse { status: "ok", database_configured: false, database_reachable: false })),
        Some(pool) => {
            let reachable = sqlx::query("SELECT 1").execute(pool).await.is_ok();
            let status = if reachable { "ok" } else { "degraded" };
            let code = if reachable { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
            (code, Json(HealthResponse { status, database_configured: true, database_reachable: reachable }))
        }
    }
}

/// Prometheus text-exposition metrics. Unauthenticated for now - see `crate::metrics` module doc.
#[utoipa::path(get, path = "/metrics", tag = "meta", responses((status = 200, body = String, content_type = "text/plain")))]
pub async fn metrics_route(State(state): State<AppState>) -> String {
    if let Some(pool) = &state.db {
        crate::metrics::DB_POOL_IN_USE.set(pool.size() as i64);
    }
    crate::metrics::render()
}

/// Triggers a re-pull of every venue.
#[utoipa::path(
    post,
    path = "/refresh",
    tag = "meta",
    responses(
        (status = 202, description = "Refresh started in the background."),
        (status = 409, description = "A refresh is already running."),
        (status = 401, description = "Missing or invalid X-Admin-Token."),
    )
)]
/// Starts a refresh and returns immediately.
///
/// A full pass fans out across every venue and category and takes a while; holding the connection
/// open for it would guarantee a gateway timeout on any real deployment. `GET /markets` reports
/// `last_refreshed_at`, which is how a caller observes completion.
pub async fn refresh(State(state): State<AppState>) -> (StatusCode, Json<&'static str>) {
    // try_lock inside `refresh()` already prevents overlapping passes; check it here too so the
    // caller gets an honest 409 rather than a 202 for work that won't happen.
    if state.aggregator.refresh_in_progress() {
        return (StatusCode::CONFLICT, Json("refresh already in progress"));
    }
    let aggregator = state.aggregator.clone();
    tokio::spawn(async move {
        aggregator.refresh().await;
    });
    (StatusCode::ACCEPTED, Json("refresh started"))
}
