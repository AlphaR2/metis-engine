//! Prometheus metrics - mirrors mobile-server's `metrics.rs` shape (a lazily-built `Registry` plus
//! named counters/gauges/histograms). Exposed at `GET /metrics`, behind the admin token.

use once_cell::sync::Lazy;
use prometheus::{
    Encoder, Histogram, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry, TextEncoder,
};

pub static REGISTRY: Lazy<Registry> = Lazy::new(Registry::new);

pub static DB_POOL_SIZE: Lazy<IntGauge> = Lazy::new(|| {
    let gauge = IntGauge::new("metis_db_pool_size", "Configured max Postgres pool connections").unwrap();
    REGISTRY.register(Box::new(gauge.clone())).expect("metric registration should not collide");
    gauge
});

pub static DB_POOL_IN_USE: Lazy<IntGauge> = Lazy::new(|| {
    let gauge = IntGauge::new("metis_db_pool_in_use", "Postgres pool connections currently checked out").unwrap();
    REGISTRY.register(Box::new(gauge.clone())).expect("metric registration should not collide");
    gauge
});

/// Markets seen on the most recent refresh, labeled by venue - a gauge (not a counter) since
/// "how many did the last refresh see" is what's useful, not a running total across restarts.
pub static VENUE_MARKETS_SEEN: Lazy<IntGaugeVec> = Lazy::new(|| {
    let gauge = IntGaugeVec::new(
        Opts::new("metis_venue_markets_seen", "Tradeable markets seen on the most recent refresh, by venue"),
        &["venue"],
    )
    .unwrap();
    REGISTRY.register(Box::new(gauge.clone())).expect("metric registration should not collide");
    gauge
});

/// Every API request, by method, matched route template (never the raw path, so ids don't explode
/// the label set) and status code.
pub static HTTP_REQUESTS: Lazy<HistogramVec> = Lazy::new(|| {
    let histogram = HistogramVec::new(
        HistogramOpts::new("metis_http_request_duration_seconds", "API request latency by route and status"),
        &["method", "route", "status"],
    )
    .unwrap();
    REGISTRY.register(Box::new(histogram.clone())).expect("metric registration should not collide");
    histogram
});

/// Wall-clock time of each full refresh pass.
pub static REFRESH_DURATION: Lazy<Histogram> = Lazy::new(|| {
    let histogram = Histogram::with_opts(
        HistogramOpts::new("metis_refresh_duration_seconds", "Duration of a full refresh across every venue")
            .buckets(vec![10.0, 30.0, 60.0, 120.0, 180.0, 240.0, 300.0, 450.0, 600.0]),
    )
    .unwrap();
    REGISTRY.register(Box::new(histogram.clone())).expect("metric registration should not collide");
    histogram
});

/// Venue call failures, by venue and kind (`rate_limited` or `error`). Business outcomes such as
/// "no liquidity" are not failures and are not counted.
pub static VENUE_ERRORS: Lazy<IntCounterVec> = Lazy::new(|| {
    let counter = IntCounterVec::new(
        Opts::new("metis_venue_errors_total", "Failed venue calls, by venue and kind"),
        &["venue", "kind"],
    )
    .unwrap();
    REGISTRY.register(Box::new(counter.clone())).expect("metric registration should not collide");
    counter
});

/// Seconds since each (venue, category) bucket last refreshed completely. Set at scrape time so
/// it is always current.
pub static BUCKET_AGE: Lazy<IntGaugeVec> = Lazy::new(|| {
    let gauge = IntGaugeVec::new(
        Opts::new("metis_bucket_age_seconds", "Seconds since the bucket's last complete refresh"),
        &["venue", "category"],
    )
    .unwrap();
    REGISTRY.register(Box::new(gauge.clone())).expect("metric registration should not collide");
    gauge
});

/// 1 when a bucket is serving its last good copy because its latest refresh failed.
pub static BUCKET_STALE: Lazy<IntGaugeVec> = Lazy::new(|| {
    let gauge = IntGaugeVec::new(
        Opts::new("metis_bucket_stale", "1 if the bucket's most recent refresh failed"),
        &["venue", "category"],
    )
    .unwrap();
    REGISTRY.register(Box::new(gauge.clone())).expect("metric registration should not collide");
    gauge
});

/// Renders the registry in Prometheus text-exposition format for `GET /metrics`.
pub fn render() -> String {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).expect("prometheus text encoding should not fail");
    String::from_utf8(buffer).expect("prometheus text encoder always emits valid utf8")
}
