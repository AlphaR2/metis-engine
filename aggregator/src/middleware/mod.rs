//! Cross-cutting HTTP concerns - CORS and operator-route auth.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::extract::Request;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use crate::state::AppState;

/// Header carrying the operator token.
pub const ADMIN_TOKEN_HEADER: &str = "x-admin-token";

/// Builds the CORS layer, restricted to exactly one configured origin.
pub fn cors_layer(origin: &str) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::exact(
            origin.parse().expect("CORS_ORIGIN must be a valid header value - checked by Config::validate()"),
        ))
        .allow_methods(Any)
        .allow_headers(Any)
}

/// Gate for operator-only routes (`POST /refresh`, `GET /metrics`).
///
/// A refresh fans out across every venue and category, so leaving it open lets any caller drive
/// our outbound rate limit; `/metrics` leaks internal state. When no `ADMIN_TOKEN` is configured
/// these routes are refused outright rather than left open - failing closed is the only safe
/// default for an endpoint whose cost is paid against someone else's API.
pub async fn require_admin_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, String)> {
    let Some(expected) = state.admin_token.as_deref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "operator routes are disabled - set ADMIN_TOKEN to enable them".to_string(),
        ));
    };
    let presented = headers.get(ADMIN_TOKEN_HEADER).and_then(|v| v.to_str().ok()).unwrap_or_default();
    // Constant-time-ish compare: length first, then bytes, so a mismatch doesn't leak the prefix
    // length through timing. Not a hardened primitive, but there's no reason to make it easy.
    let matches = presented.len() == expected.len()
        && presented.bytes().zip(expected.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
    if matches {
        Ok(next.run(request).await)
    } else {
        Err((StatusCode::UNAUTHORIZED, format!("missing or invalid {ADMIN_TOKEN_HEADER}")))
    }
}
