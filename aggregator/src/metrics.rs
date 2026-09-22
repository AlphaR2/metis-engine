//! Minimal Prometheus metrics - mirrors mobile-server's `metrics.rs` shape (a lazily-built
//! `Registry` plus a handful of named counters/gauges), sized down to what this service actually
//! has to report on day one: DB pool saturation and per-venue refresh outcomes. Exposed at
//! `GET /metrics`, unauthenticated for now - mobile-server gates its own behind an admin route,
//! worth mirroring once this service has an auth story at all.

use once_cell::sync::Lazy;
use prometheus::{Encoder, IntGauge, IntGaugeVec, Opts, Registry, TextEncoder};

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

/// Renders the registry in Prometheus text-exposition format for `GET /metrics`.
pub fn render() -> String {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).expect("prometheus text encoding should not fail");
    String::from_utf8(buffer).expect("prometheus text encoder always emits valid utf8")
}
