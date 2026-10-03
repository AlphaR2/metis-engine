//! Cross-cutting HTTP concerns - CORS, per-client rate limiting and request metrics.

use axum::extract::State;
use axum::http::{HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::Response;
use axum::extract::Request;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::state::AppState;

/// Parses `CORS_ORIGIN`: one origin or a comma-separated list, each exactly `scheme://host[:port]`.
///
/// Strict on purpose. A browser sends `Origin` with no path and no trailing slash, so an entry with
/// either would validate as a header value and then silently never match. A wildcard is refused
/// outright: this API is called from a wallet-connected page, and "any site may call it" is not a
/// posture to reach by leaving a setting at its default.
pub fn parse_origins(raw: &str) -> Result<Vec<HeaderValue>, String> {
    let entries: Vec<&str> = raw.split(',').map(str::trim).filter(|o| !o.is_empty()).collect();
    if entries.is_empty() {
        return Err("CORS_ORIGIN is empty - list at least one origin".to_string());
    }
    entries
        .into_iter()
        .map(|origin| {
            if origin == "*" || origin.contains('*') {
                return Err(format!("CORS_ORIGIN '{origin}': wildcards are not allowed - list each origin"));
            }
            let rest = origin
                .strip_prefix("https://")
                .or_else(|| origin.strip_prefix("http://"))
                .ok_or_else(|| format!("CORS_ORIGIN '{origin}' must start with http:// or https://"))?;
            if rest.is_empty() || rest.contains('/') {
                return Err(format!(
                    "CORS_ORIGIN '{origin}' must be scheme://host[:port] with no path or trailing slash"
                ));
            }
            origin.parse::<HeaderValue>().map_err(|e| format!("CORS_ORIGIN '{origin}' is not a valid header value: {e}"))
        })
        .collect()
}

/// Builds the CORS layer: any origin when none is configured, otherwise exactly the configured ones.
///
/// Methods and request headers are the ones the public API uses, not `Any`. Response headers a
/// browser client needs are exposed explicitly: `Retry-After` is not CORS-safelisted, so without
/// this a page cannot read how long to back off after a 429, and `ETag` likewise.
pub fn cors_layer(origins: Option<&str>) -> CorsLayer {
    let allow = match origins {
        Some(raw) => AllowOrigin::list(parse_origins(raw).expect("CORS_ORIGIN checked by Config::validate()")),
        None => AllowOrigin::any(),
    };
    CorsLayer::new()
        .allow_origin(allow)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::ACCEPT, header::IF_NONE_MATCH])
        .expose_headers([header::RETRY_AFTER, header::ETAG])
        .max_age(std::time::Duration::from_secs(600))
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
/// `/v1/route` or `/v1/markets/{venue}/{id}/live`) costs `LIVE_QUOTE_COST`, since that is what actually
/// spends our venue rate limits.
pub struct RateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    trust_forwarded_for: bool,
    /// Proxies in front of this service that each append to `X-Forwarded-For`.
    proxy_hops: usize,
    buckets: std::sync::Mutex<std::collections::HashMap<std::net::IpAddr, (f64, std::time::Instant)>>,
}

/// Tokens a live-quoting request costs.
const LIVE_QUOTE_COST: f64 = 10.0;

/// Tokens a status poll costs: cheap enough to poll every few seconds, not free.
const STATUS_POLL_COST: f64 = 2.0;

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
            proxy_hops: 1,
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
                .and_then(|v| forwarded_client(v, self.proxy_hops));
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

/// The client address in an `X-Forwarded-For` value, given how many trusted proxies appended to it.
///
/// Each proxy appends the address it received the request from, so the entries a client can forge
/// sit on the **left** and the ones our own proxies wrote sit on the **right**. Taking the first
/// entry, as this used to, lets any client pick its own rate-limit identity behind a proxy that
/// appends rather than overwrites. With `hops` trusted proxies the client is the `hops`-th entry
/// from the right. Fewer entries than hops means the header can't be trusted, so it yields `None`
/// and the caller falls back to the peer address.
fn forwarded_client(header: &str, hops: usize) -> Option<std::net::IpAddr> {
    let entries: Vec<&str> = header.split(',').map(str::trim).collect();
    let index = entries.len().checked_sub(hops.max(1))?;
    entries.get(index)?.parse().ok()
}

/// What a request costs against the bucket, by how much venue work it triggers.
///
/// - live quotes (`POST /v1/route`, `/live`) fan out to venues: 10
/// - building a transaction (`POST /execution/*`) and reading a wallet's positions call a venue
///   each time: 10
/// - polling an action's status calls one venue but is expected to repeat every few seconds: 2
/// - everything else is served from memory: 1
fn request_cost(request: &Request) -> f64 {
    let uri = request.uri();
    let path = uri.path();
    // `/v1/route` quotes every listing of an outcome live; it used to cost 1 like a memory read.
    let quotes_live = path == "/v1/route" || path.ends_with("/live");
    if quotes_live {
        return LIVE_QUOTE_COST;
    }
    if path == "/v1/positions" || (path.starts_with("/execution/") && request.method() == Method::POST) {
        return LIVE_QUOTE_COST;
    }
    if path.starts_with("/execution/") && path != "/execution/abilities" {
        return STATUS_POLL_COST;
    }
    1.0
}

pub async fn rate_limit(State(state): State<AppState>, request: Request, next: Next) -> Response {
    use axum::response::IntoResponse;

    let limiter = &state.rate_limiter;
    let client = limiter.client_ip(&request);
    match limiter.try_take(client, request_cost(&request), std::time::Instant::now()) {
        Ok(()) => next.run(request).await,
        Err(retry_after) => {
            let mut response =
                crate::errors::ApiError::rate_limited("rate limit exceeded - slow down").into_response();
            if let Ok(value) = HeaderValue::from_str(&retry_after.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
            response
        }
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
        assert_eq!(cost(Method::GET, "/v1/events?limit=5"), 1.0);
        assert_eq!(cost(Method::POST, "/v1/route"), LIVE_QUOTE_COST, "routing quotes every venue live");
        assert_eq!(cost(Method::GET, "/v1/markets/jupiter/M1/live"), LIVE_QUOTE_COST);
    }

    fn cost(method: Method, uri: &str) -> f64 {
        request_cost(&Request::builder().method(method).uri(uri).body(axum::body::Body::empty()).unwrap())
    }

    #[test]
    fn venue_backed_routes_cost_more_than_memory_reads() {
        assert_eq!(cost(Method::POST, "/execution/orders"), LIVE_QUOTE_COST);
        assert_eq!(cost(Method::POST, "/execution/confirm"), LIVE_QUOTE_COST);
        assert_eq!(cost(Method::GET, "/v1/positions?wallet=W"), LIVE_QUOTE_COST);
        assert_eq!(cost(Method::GET, "/execution/jupiter/ORDER"), STATUS_POLL_COST);
        assert_eq!(cost(Method::GET, "/execution/abilities"), 1.0, "the capability check is a memory read");
        assert_eq!(cost(Method::GET, "/v1/events"), 1.0);
    }

    #[test]
    fn a_status_poll_every_two_seconds_stays_inside_the_default_budget() {
        // 300/min default: 30 polls a minute at 2 each is 60 tokens, leaving room for browsing.
        assert!(30.0 * STATUS_POLL_COST < 300.0 / 2.0);
    }

    #[test]
    fn the_client_is_the_nth_entry_from_the_right() {
        let ip = |s: &str| -> Option<IpAddr> { Some(s.parse().unwrap()) };
        // One proxy that appended the peer it saw.
        assert_eq!(forwarded_client("203.0.113.9", 1), ip("203.0.113.9"));
        // A client forging a leftmost entry does not become the rate-limit identity.
        assert_eq!(forwarded_client("1.2.3.4, 203.0.113.9", 1), ip("203.0.113.9"));
        assert_eq!(forwarded_client("6.6.6.6, 1.2.3.4, 203.0.113.9", 1), ip("203.0.113.9"));
        // CDN then load balancer: the client is two from the right.
        assert_eq!(forwarded_client("203.0.113.9, 198.51.100.7", 2), ip("203.0.113.9"));
        assert_eq!(forwarded_client("6.6.6.6, 203.0.113.9, 198.51.100.7", 2), ip("203.0.113.9"));
    }

    #[test]
    fn a_header_shorter_than_the_proxy_chain_is_not_trusted() {
        assert_eq!(forwarded_client("203.0.113.9", 2), None);
        assert_eq!(forwarded_client("", 1), None);
        assert_eq!(forwarded_client("not-an-ip", 1), None);
    }

    #[test]
    fn a_forged_forwarded_header_cannot_dodge_the_limit() {
        let limiter = RateLimiter::new(1, true);
        let request = |xff: &str| {
            Request::builder().uri("/v1/events").header("x-forwarded-for", xff).body(axum::body::Body::empty()).unwrap()
        };
        let t0 = Instant::now();
        // Same real client (rightmost), a fresh forged identity each time.
        let first = limiter.client_ip(&request("9.9.9.1, 203.0.113.9"));
        let second = limiter.client_ip(&request("9.9.9.2, 203.0.113.9"));
        assert_eq!(first, second);
        assert!(limiter.try_take(first, 1.0, t0).is_ok());
        assert!(limiter.try_take(second, 1.0, t0).is_err());
    }

    #[test]
    fn origins_parse_as_a_list_and_reject_anything_a_browser_would_never_send() {
        assert_eq!(parse_origins("https://a.example, https://b.example:8443").unwrap().len(), 2);
        assert!(parse_origins("https://a.example/").is_err(), "trailing slash never matches Origin");
        assert!(parse_origins("*").is_err());
        assert!(parse_origins("https://*.a.example").is_err());
        assert!(parse_origins("ftp://a.example").is_err());
        assert!(parse_origins("").is_err());
    }
}
