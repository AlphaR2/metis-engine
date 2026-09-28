//! Cross-cutting HTTP concerns - CORS, operator-route auth, per-client rate limiting and request
//! metrics.

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

/// Records every request's latency under its matched route template.
pub async fn record_http_metrics(request: Request, next: Next) -> Response {
    let method = request.method().to_string();
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or_else(|| "unmatched".to_string(), |p| p.as_str().to_string());
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    crate::metrics::HTTP_REQUESTS
        .with_label_values(&[method.as_str(), route.as_str(), response.status().as_str()])
        .observe(started.elapsed().as_secs_f64());
    response
}

/// Per-client token bucket for the public routes (audit A5).
///
/// Each client IP holds up to `capacity` tokens, refilled continuously at `capacity` per minute.
/// A plain request costs one token; one that fans out to live venue quotes (`quote_size=` on
/// `/markets`, or `/find-best-market`) costs `LIVE_QUOTE_COST`, since that is what actually
/// spends our venue rate limits.
pub struct RateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    trust_forwarded_for: bool,
    buckets: std::sync::Mutex<std::collections::HashMap<std::net::IpAddr, (f64, std::time::Instant)>>,
}

/// Tokens a live-quoting request costs.
const LIVE_QUOTE_COST: f64 = 10.0;

/// Clients tracked before idle ones are swept.
const MAX_TRACKED_CLIENTS: usize = 10_000;

impl RateLimiter {
    /// `per_minute` of 0 disables limiting. `trust_forwarded_for` should only be set behind a
    /// proxy that overwrites `X-Forwarded-For`; otherwise a client can claim any address.
    pub fn new(per_minute: u32, trust_forwarded_for: bool) -> Self {
        Self {
            capacity: f64::from(per_minute),
            refill_per_sec: f64::from(per_minute) / 60.0,
            trust_forwarded_for,
            buckets: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Takes `cost` tokens for `client` if it has them; otherwise returns how many seconds until
    /// it will.
    fn try_take(&self, client: std::net::IpAddr, cost: f64, now: std::time::Instant) -> Result<(), u64> {
        if self.capacity <= 0.0 {
            return Ok(());
        }
        let cost = cost.min(self.capacity);
        let mut buckets = self.buckets.lock().expect("rate limiter lock poisoned");
        if buckets.len() >= MAX_TRACKED_CLIENTS {
            // A client whose bucket has refilled completely is indistinguishable from a new one.
            let (capacity, refill) = (self.capacity, self.refill_per_sec);
            buckets.retain(|_, (tokens, at)| *tokens + now.duration_since(*at).as_secs_f64() * refill < capacity);
        }
        let (tokens, at) = buckets.entry(client).or_insert((self.capacity, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f64() * self.refill_per_sec).min(self.capacity);
        *at = now;
        if *tokens >= cost {
            *tokens -= cost;
            Ok(())
        } else {
            Err(((cost - *tokens) / self.refill_per_sec).ceil() as u64)
        }
    }

    fn client_ip(&self, request: &Request) -> std::net::IpAddr {
        if self.trust_forwarded_for {
            let forwarded = request
                .headers()
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .and_then(|v| v.trim().parse().ok());
            if let Some(ip) = forwarded {
                return ip;
            }
        }
        request
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map_or(std::net::IpAddr::from([0, 0, 0, 0]), |info| info.0.ip())
    }
}

fn request_cost(request: &Request) -> f64 {
    let uri = request.uri();
    let quotes_live = uri.path() == "/find-best-market"
        || uri.query().is_some_and(|q| q.split('&').any(|pair| pair.starts_with("quote_size=")));
    if quotes_live { LIVE_QUOTE_COST } else { 1.0 }
}

pub async fn rate_limit(State(state): State<AppState>, request: Request, next: Next) -> Response {
    use axum::response::IntoResponse;

    let limiter = &state.rate_limiter;
    let client = limiter.client_ip(&request);
    match limiter.try_take(client, request_cost(&request), std::time::Instant::now()) {
        Ok(()) => next.run(request).await,
        Err(retry_after) => (
            StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, retry_after.to_string())],
            "rate limit exceeded - slow down",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;
    use std::time::{Duration, Instant};

    const CLIENT: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1));

    #[test]
    fn a_client_is_limited_after_its_capacity_and_refills_over_time() {
        let limiter = RateLimiter::new(60, false);
        let t0 = Instant::now();
        for _ in 0..60 {
            assert!(limiter.try_take(CLIENT, 1.0, t0).is_ok());
        }
        assert_eq!(limiter.try_take(CLIENT, 1.0, t0), Err(1));
        assert!(limiter.try_take(CLIENT, 1.0, t0 + Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn clients_are_limited_independently() {
        let limiter = RateLimiter::new(1, false);
        let t0 = Instant::now();
        assert!(limiter.try_take(CLIENT, 1.0, t0).is_ok());
        assert!(limiter.try_take(CLIENT, 1.0, t0).is_err());
        assert!(limiter.try_take(IpAddr::from([10, 0, 0, 2]), 1.0, t0).is_ok());
    }

    #[test]
    fn zero_disables_limiting() {
        let limiter = RateLimiter::new(0, false);
        for _ in 0..1_000 {
            assert!(limiter.try_take(CLIENT, LIVE_QUOTE_COST, Instant::now()).is_ok());
        }
    }

    #[test]
    fn live_quoting_requests_cost_more() {
        let plain = Request::builder().uri("/markets?category=sports").body(axum::body::Body::empty()).unwrap();
        let quoted = Request::builder().uri("/markets?category=sports&quote_size=10").body(axum::body::Body::empty()).unwrap();
        assert_eq!(request_cost(&plain), 1.0);
        assert_eq!(request_cost(&quoted), LIVE_QUOTE_COST);
    }
}
