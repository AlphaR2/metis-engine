//! Server construction and wiring - builds every dependency, assembles the router, and serves.
//! `main.rs` is process bootstrap only; this is where the actual service lives.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::aggregator::Aggregator;
use crate::api::v1;
use crate::config::Config;
use crate::database;
use crate::middleware::{RateLimiter, cors_layer, rate_limit, record_http_metrics};
use crate::openapi::ApiDoc;
use crate::models::venue::CategoryAliases;
use crate::services::registry::build_venues;
use crate::state::AppState;

/// Builds every dependency, wires the router, and serves forever.
pub async fn run(config: Config) {
    let venues = build_venues(&config.venues);
    tracing::info!("enabled venues: {:?}", venues.iter().map(|v| v.adapter.id()).collect::<Vec<_>>());
    let aliases = CategoryAliases::defaults().with_overrides(&config.category_aliases);


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

    let aggregator = Arc::new(Aggregator::new(venues, aliases, db.clone()));

    // Restore the previous catalog, prime it with a refresh, then keep it fresh on a timer - all in
    // the background, so none of it blocks the server. Restoring a large catalog from a remote
    // Postgres takes tens of seconds, and a full refresh minutes; health and swagger shouldn't wait
    // on either, and a restart shouldn't look like an outage. The warm start runs first in the same
    // task, so it can never land on top of fresher data from the boot refresh. `/ready` reports
    // 503 until a catalog is held, so a caller can tell an empty catalog from an unprimed one.
    let background_aggregator = aggregator.clone();
    let refresh_interval = config.refresh_interval_secs;
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        let restored = background_aggregator.warm_start().await;
        if restored > 0 {
            tracing::info!("warm start: restored {restored} markets from Postgres in {:.1}s", started.elapsed().as_secs_f64());
        }

        let mut ticker = tokio::time::interval(Duration::from_secs(refresh_interval));
        // A pass can outlast the interval. The default `Burst` would then fire the missed ticks
        // back-to-back and refresh continuously; `Delay` waits a full interval after a late tick.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick fires immediately, which is exactly the boot-time prime.
        loop {
            ticker.tick().await;
            // On a failed boot (network down for a minute), retry soon rather than in a full interval.
            background_aggregator.refresh_until_held(Duration::from_secs(60), 10).await;
        }
    });
    tracing::info!("background refresh every {refresh_interval}s");

    let state = AppState {
        aggregator,
        db,
        execution_enabled: config.write_mode,
        market_creation_enabled: config.write_mode,
        // Per-IP limiting is switched off for the MVP; the limiter stays in the stack for later.
        rate_limiter: Arc::new(RateLimiter::new(0, false)),
    };

    tracing::info!("mode: {}", if config.write_mode { "write (builds transactions)" } else { "read (catalog only)" });

    let app = build_router(state, config.cors_origin.as_deref(), Duration::from_secs(config.request_timeout_secs));

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", config.port))
        .await
        .unwrap_or_else(|e| panic!("failed to bind port {}: {e}", config.port));

    tracing::info!("metis-api listening on :{} - swagger UI at /swagger-ui", config.port);
    // Connect info carries the peer address the rate limiter keys on.
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
    tracing::info!("metis-api stopped");
}

/// Assembles the full router around `state`. Split out of `run` so route tests exercise exactly
/// what production serves - same gating, same layers.
pub fn build_router(state: AppState, cors_origin: Option<&str>, request_timeout: Duration) -> Router {
    // Public routes carry the (currently disabled) per-client limiter; operator routes do not.
    let admin_routes = v1::route::configure_admin();
    let public_routes =
        v1::route::configure().route_layer(axum::middleware::from_fn_with_state(state.clone(), rate_limit));

    Router::new()
        .merge(public_routes)
        .merge(admin_routes)
        .route_layer(axum::middleware::from_fn(record_http_metrics))
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        // A request that outlives this is answered 408 and its work dropped, so one slow venue
        // can't hold a connection (and whatever it holds) open indefinitely.
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, request_timeout))
        .layer(cors_layer(cors_origin))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Resolves on Ctrl-C or SIGTERM, letting in-flight requests finish before the process exits.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::warn!("could not listen for Ctrl-C: {err}");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::warn!("could not listen for SIGTERM: {err}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received - draining in-flight requests");
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::ScriptedVenue;

    struct Harness {
        #[allow(dead_code)]
        venue: Arc<ScriptedVenue>,
        state: AppState,
    }

    impl Harness {
        async fn new(rate_limit_per_minute: u32) -> Self {
            let venue = Arc::new(ScriptedVenue::new(true, 2));
            let aggregator = Arc::new(Aggregator::new(vec![venue.registered()], CategoryAliases::defaults(), None));
            aggregator.refresh().await;
            let state = AppState {
                aggregator,
                db: None,
                execution_enabled: false,
                market_creation_enabled: false,
                rate_limiter: Arc::new(RateLimiter::new(rate_limit_per_minute, false)),
            };
            Self { venue, state }
        }

        fn router(&self) -> Router {
            build_router(self.state.clone(), Some("http://localhost:3000"), Duration::from_secs(5))
        }

        async fn send(&self, request: Request<Body>) -> (StatusCode, String) {
            let response = self.router().oneshot(request).await.unwrap();
            let status = response.status();
            let body = response.into_body().collect().await.unwrap().to_bytes();
            (status, String::from_utf8_lossy(&body).to_string())
        }

        async fn get(&self, uri: &str) -> (StatusCode, String) {
            self.send(Request::get(uri).body(Body::empty()).unwrap()).await
        }

    }

    #[tokio::test(start_paused = true)]
    async fn a_client_over_its_limit_gets_429_with_retry_after() {
        let h = Harness::new(3).await;
        for _ in 0..3 {
            assert_eq!(h.get("/v1/venues").await.0, StatusCode::OK);
        }
        let response = h.router().oneshot(Request::get("/v1/venues").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(response.headers().contains_key("retry-after"));
    }

    #[tokio::test]
    async fn fill_score_quotes_the_call_and_ranks_it() {
        let h = Harness::new(0).await;
        let (status, body) = h.get("/v1/fill-score?venue_id=scripted&venue_market_id=g1-p0-m0&side=yes&amount_usdc=10").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let score: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(score["side"], "yes");
        assert_eq!(score["amount_usdc"], "10");
        assert!(score["method"].as_str().unwrap().contains("Fill Score"), "the formula travels with the answer");
        let options = score["options"].as_array().unwrap();
        assert!(!options.is_empty());
        let first = &options[0];
        assert_eq!(first["venue_id"], "scripted");
        assert_eq!(first["quoted"], true);
        // The scripted venue quotes 20 shares for 10 USDC and reports no liquidity: fully filled, depth unknown.
        assert_eq!(first["score"], 100);
        assert_eq!(first["other_side_usdc"], "10");
        assert!(first["depth_usdc"].is_null());
    }

    #[tokio::test]
    async fn fill_score_refuses_bad_input_and_unknown_markets() {
        let h = Harness::new(0).await;
        assert_eq!(h.get("/v1/fill-score?venue_id=scripted&venue_market_id=nope").await.0, StatusCode::NOT_FOUND);
        assert_eq!(h.get("/v1/fill-score?venue_id=scripted&venue_market_id=g1-p0-m0&side=maybe").await.0, StatusCode::BAD_REQUEST);
        for amount in ["0", "-5", "ten"] {
            let uri = format!("/v1/fill-score?venue_id=scripted&venue_market_id=g1-p0-m0&amount_usdc={amount}");
            assert_eq!(h.get(&uri).await.0, StatusCode::BAD_REQUEST, "{amount}");
        }
    }

    /// The real venues, replayed from fixtures, behind the real router.
    struct FixtureHarness {
        router: Router,
        _servers: Vec<crate::test_support::FixtureServer>,
    }

    impl FixtureHarness {
        async fn new() -> Self {
            let (aggregator, servers) = crate::venue_contract::fixture_aggregator().await;
            let state = AppState {
                aggregator: Arc::new(aggregator),
                db: None,
                execution_enabled: false,
                market_creation_enabled: false,
                rate_limiter: Arc::new(RateLimiter::new(0, false)),
            };
            Self { router: build_router(state, Some("http://localhost:3000"), Duration::from_secs(30)), _servers: servers }
        }

        async fn send(&self, request: Request<Body>) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
            let response = self.router.clone().oneshot(request).await.unwrap();
            let (status, headers) = (response.status(), response.headers().clone());
            let body = response.into_body().collect().await.unwrap().to_bytes();
            (status, headers, serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null))
        }

        async fn get(&self, uri: &str) -> (StatusCode, serde_json::Value) {
            let (status, _, body) = self.send(Request::get(uri).body(Body::empty()).unwrap()).await;
            (status, body)
        }
    }

    #[tokio::test]
    async fn v1_events_list_pages_and_every_card_previews_its_markets() {
        let h = FixtureHarness::new().await;
        let (status, first) = h.get("/v1/events?limit=5").await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let events = first["events"].as_array().unwrap();
        assert_eq!(events.len(), 5);
        assert!(first["total"].as_u64().unwrap() > 100);
        for event in events {
            assert!(event["id"].as_str().unwrap().starts_with("evt_"));
            assert!(!event["title"].as_str().unwrap().is_empty());
            assert!(event["market_count"].as_u64().unwrap() >= 1);
            assert!(event["rating"].as_u64().is_some());
            assert!(event["grouping"].as_str().is_some());
            let top = event["top_markets"].as_array().unwrap();
            assert!(!top.is_empty() && top.len() <= 3);
            assert!(!top[0]["title"].as_str().unwrap().is_empty(), "the venue's own market title");
        }
        let cursor = first["next_cursor"].as_str().unwrap();
        let (_, second) = h.get(&format!("/v1/events?limit=5&cursor={cursor}")).await;
        assert_ne!(second["events"][0]["id"], events[0]["id"], "the next page starts after the first");
    }

    #[tokio::test]
    async fn v1_lists_answer_304_to_a_matching_etag() {
        let h = FixtureHarness::new().await;
        let (_, headers, _) = h.send(Request::get("/v1/events?limit=3").body(Body::empty()).unwrap()).await;
        let etag = headers.get("etag").unwrap().to_str().unwrap().to_string();
        let (status, _, _) = h.send(Request::get("/v1/events?limit=3").header("if-none-match", &etag).body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn v1_event_detail_holds_the_venues_markets_untouched_and_rated() {
        let h = FixtureHarness::new().await;
        let (_, list) = h.get("/v1/events?limit=1&sort=volume").await;
        let id = list["events"][0]["id"].as_str().unwrap().to_string();

        let (status, event) = h.get(&format!("/v1/events/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        let markets = event["markets"].as_array().unwrap();
        assert_eq!(markets.len() as u64, event["market_count"].as_u64().unwrap());
        for node in markets {
            assert!(node["market"]["venue_market_id"].as_str().is_some(), "{node}");
            assert!(node["market"]["title"].as_str().is_some_and(|t| !t.is_empty()));
            assert!(node["rating"]["score"].as_u64().is_some());
            assert!(!node["rating"]["breakdown"].as_array().unwrap().is_empty());
        }
        assert!(event["grouping"]["reason"].as_str().is_some());

        let (status, by_volume) = h.get(&format!("/v1/events/{id}?sort=volume")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(by_volume["markets"].as_array().unwrap().len(), markets.len(), "sorting never drops a market");
        assert_eq!(h.get(&format!("/v1/events/{id}?sort=nope")).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(h.get("/v1/events/evt_0000000000000000").await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn v1_markets_always_carry_their_event() {
        let h = FixtureHarness::new().await;
        let (status, page) = h.get("/v1/markets?limit=25").await;
        assert_eq!(status, StatusCode::OK);
        for market in page["markets"].as_array().unwrap() {
            assert!(market["event_id"].as_str().is_some_and(|id| id.starts_with("evt_")), "{market}");
            assert!(!market["display_title"].as_str().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn v1_categories_and_venues_come_from_the_venues() {
        let h = FixtureHarness::new().await;
        let (_, categories) = h.get("/v1/categories").await;
        let science = categories.as_array().unwrap().iter().find(|c| c["category"] == "science").expect("science present");
        assert_eq!(science["venues"][0]["native"][0], "climate & science", "Jupiter's own name is kept");
        let (_, venues) = h.get("/v1/venues").await;
        let ids: Vec<&str> = venues.as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["panta", "jupiter"]);
    }

    #[tokio::test]
    async fn v1_rejects_unknown_filters() {
        let h = FixtureHarness::new().await;
        assert_eq!(h.get("/v1/events?sort=nope").await.0, StatusCode::BAD_REQUEST);
        assert_eq!(h.get("/v1/events?status=nope").await.0, StatusCode::BAD_REQUEST);
        assert_eq!(h.get("/v1/events?cursor=garbage").await.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_live_market_read_names_an_unknown_venue_and_never_serves_a_missing_market() {
        let h = FixtureHarness::new().await;
        assert_eq!(h.get("/v1/markets/nowhere/x/live").await.0, StatusCode::NOT_FOUND);
        let (status, _) = h.get("/v1/markets/jupiter/no-such-market/live").await;
        assert!(status == StatusCode::NOT_FOUND || status == StatusCode::BAD_GATEWAY, "got {status}");
        assert!(!h.get("/v1/markets/jupiter/..%2Fadmin/live").await.0.is_success());
    }

    #[tokio::test(start_paused = true)]
    async fn health_is_always_200_but_ready_waits_for_a_catalog() {
        let venue = Arc::new(ScriptedVenue::new(true, 1));
        let aggregator = Arc::new(Aggregator::new(vec![venue.registered()], CategoryAliases::defaults(), None));
        let state = AppState {
            aggregator: aggregator.clone(),
            db: None,
            execution_enabled: false,
            market_creation_enabled: false,
            rate_limiter: Arc::new(RateLimiter::new(0, false)),
        };
        let status = |uri: &'static str| {
            let router = build_router(state.clone(), Some("http://localhost:3000"), Duration::from_secs(5));
            async move { router.oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap().status() }
        };

        assert_eq!(status("/health").await, StatusCode::OK, "liveness never depends on the catalog");
        assert_eq!(status("/ready").await, StatusCode::SERVICE_UNAVAILABLE, "nothing held yet");
        aggregator.refresh().await;
        assert_eq!(status("/ready").await, StatusCode::OK);
    }

    // ── Frontend contract: stable error codes, rate-limit and CORS headers, positions ─────────

    fn json(body: &str) -> serde_json::Value {
        serde_json::from_str(body).unwrap_or_else(|e| panic!("not json ({e}): {body}"))
    }

    #[tokio::test(start_paused = true)]
    async fn a_bad_wallet_is_refused_with_a_code_a_client_can_route_on() {
        let h = Harness::new(0).await;
        let (status, body) = h.get("/v1/positions?wallet=not-a-wallet").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let body = json(&body);
        assert_eq!(body["code"], "INVALID_REQUEST");
        assert_eq!(body["retryable"], false);
        assert!(body["error"].as_str().unwrap().contains("wallet"));
    }

    #[tokio::test(start_paused = true)]
    async fn positions_for_a_valid_wallet_with_no_capable_venue_is_an_honest_empty_list() {
        let h = Harness::new(0).await;
        let (status, body) = h.get(&format!("/v1/positions?wallet={}", crate::utils::solana_tx::SYSTEM_PROGRAM)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let body = json(&body);
        assert_eq!(body["positions"], serde_json::json!([]));
        assert_eq!(body["venues_failed"], serde_json::json!([]));
        assert_eq!(body["venues_ok"], serde_json::json!([]));
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_execution_answers_each_action_with_its_own_code() {
        let h = Harness::new(0).await;
        for (path, body, code) in [
            ("/execution/orders", r#"{"venue_id":"scripted","venue_market_id":"m","side":"yes","amount_usdc":"10","wallet":"W"}"#, "EXECUTION_DISABLED"),
            ("/execution/sells", r#"{"venue_id":"scripted","venue_ref":"p","wallet":"W"}"#, "EXECUTION_DISABLED"),
            ("/execution/claims", r#"{"venue_id":"scripted","kind":"claim_winnings","venue_ref":"claim:W:p","wallet":"W"}"#, "EXECUTION_DISABLED"),
            (
                "/execution/markets",
                r#"{"venue_id":"scripted","question":"q","resolution_rule":"r","sources_of_truth":["s"],"category":"c","start_time":1,"end_time":2,"resolution_time":3,"image_url":"u","wallet":"W"}"#,
                "MARKET_CREATION_DISABLED",
            ),
        ] {
            let request = Request::post(path).header("content-type", "application/json").body(Body::from(body)).unwrap();
            let (status, text) = h.send(request).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {text}");
            assert_eq!(json(&text)["code"], code, "{path}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_unknown_venue_has_its_own_code() {
        let mut h = Harness::new(0).await;
        h.state.execution_enabled = true;
        let request = Request::post("/execution/orders")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"venue_id":"nope","venue_market_id":"m","side":"yes","amount_usdc":"10","wallet":"W"}"#))
            .unwrap();
        let (status, text) = h.send(request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert_eq!(json(&text)["code"], "UNKNOWN_VENUE");
    }

    #[tokio::test(start_paused = true)]
    async fn a_venue_that_cannot_do_an_action_says_unsupported_not_outage() {
        let mut h = Harness::new(0).await;
        h.state.execution_enabled = true;
        let request = Request::post("/execution/sells")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"venue_id":"scripted","venue_ref":"p","wallet":"W"}"#))
            .unwrap();
        let (status, text) = h.send(request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        let body = json(&text);
        assert_eq!(body["code"], "UNSUPPORTED");
        assert_eq!(body["retryable"], false);
    }

    #[tokio::test(start_paused = true)]
    async fn a_rate_limited_client_gets_json_with_a_code_and_a_readable_retry_after() {
        let h = Harness::new(1).await;
        let first = h.router().oneshot(Request::get("/v1/venues").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        let second = h
            .router()
            .oneshot(Request::get("/v1/venues").header("origin", "http://localhost:3000").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
        let headers = second.headers().clone();
        assert!(headers.get("retry-after").is_some(), "the client needs to know how long to wait");
        let exposed = headers.get("access-control-expose-headers").and_then(|v| v.to_str().ok()).unwrap_or_default().to_lowercase();
        assert!(exposed.contains("retry-after"), "a browser can only read Retry-After if it is exposed: {exposed:?}");
        let body = second.into_body().collect().await.unwrap().to_bytes();
        let body = json(&String::from_utf8_lossy(&body));
        assert_eq!(body["code"], "RATE_LIMITED");
        assert_eq!(body["retryable"], true);
    }

    #[tokio::test(start_paused = true)]
    async fn only_the_configured_origin_is_allowed() {
        let h = Harness::new(0).await;
        let allowed = h
            .router()
            .oneshot(Request::get("/v1/venues").header("origin", "http://localhost:3000").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(allowed.headers().get("access-control-allow-origin").map(|v| v.to_str().unwrap()), Some("http://localhost:3000"));

        let stranger = h
            .router()
            .oneshot(Request::get("/v1/venues").header("origin", "https://evil.example").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(stranger.headers().get("access-control-allow-origin").is_none(), "an unlisted origin must not be echoed back");
    }

    #[tokio::test(start_paused = true)]
    async fn the_capability_flags_reach_the_client() {
        let h = Harness::new(0).await;
        let (status, body) = h.get("/execution/abilities").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let venue = &json(&body)["venues"][0];
        for flag in ["can_execute", "can_create_markets", "can_list_positions", "can_claim", "can_sell"] {
            assert!(venue.get(flag).is_some(), "missing {flag}");
        }
    }
}

