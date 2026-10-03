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

/// How long a health probe waits on the database before calling it unreachable.
const DB_PING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Liveness check: the process is up and answering. **Always 200.** A database outage does not stop
/// this service serving (every read comes from memory), so failing this probe would only get the
/// process restarted, and a restart drops the in-memory catalog while the database is still down.
/// The body reports the database state for a human or a dashboard; use `/ready` to gate traffic.
#[utoipa::path(get, path = "/health", tag = "meta", responses((status = 200, body = HealthResponse)))]
pub async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(match &state.db {
        None => HealthResponse { status: "ok", database_configured: false, database_reachable: false },
        Some(pool) => {
            let reachable = matches!(
                tokio::time::timeout(DB_PING_TIMEOUT, sqlx::query("SELECT 1").execute(pool)).await,
                Ok(Ok(_))
            );
            HealthResponse {
                status: if reachable { "ok" } else { "degraded" },
                database_configured: true,
                database_reachable: reachable,
            }
        }
    })
}

/// Response body for `GET /ready`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ReadyResponse {
    pub ready: bool,
    /// Markets currently held in memory.
    pub markets: usize,
    /// Venues whose catalog is a last good copy because their latest refresh failed.
    pub stale_venues: Vec<String>,
}

/// Readiness check: whether this instance has events to serve. **503 until the first venue catalog
/// is held and the event graph built from it** (from a warm start out of Postgres or a completed
/// refresh), so a load balancer or a UI does not meet an empty board while the graph is still building. A stale
/// venue does not make it unready: serving the last good catalog is the designed behaviour.
#[utoipa::path(get, path = "/ready", tag = "meta", responses((status = 200, body = ReadyResponse), (status = 503, body = ReadyResponse)))]
pub async fn ready(State(state): State<AppState>) -> (StatusCode, Json<ReadyResponse>) {
    let store = state.aggregator.markets.read().await;
    let markets = store.len();
    let mut stale_venues: Vec<String> =
        store.bucket_statuses().into_iter().filter(|b| b.stale).map(|b| b.venue_id).collect();
    stale_venues.sort();
    stale_venues.dedup();
    let built = state.aggregator.events.read().await.built_at > 0;
    let ready = markets > 0 && built;
    let code = if ready { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (code, Json(ReadyResponse { ready, markets, stale_venues }))
}

/// Prometheus text-exposition metrics, behind the admin token.
#[utoipa::path(get, path = "/metrics", tag = "meta", responses((status = 200, body = String, content_type = "text/plain")))]
pub async fn metrics_route(State(state): State<AppState>) -> String {
    if let Some(pool) = &state.db {
        // `size()` is connections open, not in use; idle ones are the difference.
        crate::metrics::DB_POOL_IN_USE.set(pool.size() as i64 - pool.num_idle() as i64);
    }
    let now = crate::utils::time::now_unix();
    for bucket in state.aggregator.markets.read().await.bucket_statuses() {
        let labels = [bucket.venue_id.as_str(), bucket.category.as_str()];
        crate::metrics::BUCKET_AGE.with_label_values(&labels).set(now - bucket.refreshed_at);
        crate::metrics::BUCKET_STALE.with_label_values(&labels).set(i64::from(bucket.stale));
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
/// open for it would guarantee a gateway timeout on any real deployment. `GET /v1/venues`
/// reports each venue's staleness, which is how a caller observes completion.
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
