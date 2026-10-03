//! The venue contract: checks every adapter must pass, run against its recorded fixtures.
//!
//! Each venue is started unmodified against a `FixtureServer` replaying its captured responses,
//! then driven through the real refresh pipeline (`Aggregator::refresh`), so what is checked is
//! exactly what production would store. A new venue is "done" when it has fixtures
//! (`scripts/capture-fixtures.sh`), a row in `VENUES` below, and passes every test here.
//!
//! The clock is frozen at capture time so the recorded markets don't expire as real time passes.

use std::collections::HashSet;
use std::sync::Arc;

use rust_decimal::Decimal;

use crate::aggregator::Aggregator;
use crate::models::venue::{CategoryAliases, VenueMarket};
use crate::services::VenueAdapter;
use crate::services::registry::{RegisteredVenue, VenueSettings};
use crate::test_support::{FixtureServer, fixtures_captured_at};
use crate::utils::time::freeze_now_for_this_thread;

/// (fixture directory, API base path on the venue's host, adapter factory).
type VenueFactory = fn(String) -> Arc<dyn VenueAdapter>;
const VENUES: &[(&str, &str, VenueFactory)] = &[
    ("panta", "/api/v1", |base| {
        Arc::new(crate::services::panta::PantaAdapter::new(base, "fixture-key".into(), "fixture-wallet".into(), None))
    }),
    ("jupiter", "/prediction/v1", |base| Arc::new(crate::services::jupiter::JupiterAdapter::new(base, "fixture-key".into()))),
];

/// One venue's adapter and an aggregator that has refreshed it once from fixtures.
struct Refreshed {
    adapter: Arc<dyn VenueAdapter>,
    aggregator: Aggregator,
    _server: FixtureServer,
}

impl Refreshed {
    async fn new(venue_dir: &str, base_path: &str, factory: VenueFactory) -> Self {
        freeze_now_for_this_thread(fixtures_captured_at());
        let server = FixtureServer::start(venue_dir).await;
        let adapter = factory(format!("{}{base_path}", server.base));
        // Fixtures hold each chain's first page only.
        let settings = VenueSettings { page_cap: 1, min_request_gap: std::time::Duration::ZERO, ..VenueSettings::default() };
        let aggregator = Aggregator::new(
            vec![RegisteredVenue { adapter: adapter.clone(), settings }],
            CategoryAliases::defaults(),
            None,
        );
        assert!(aggregator.refresh().await);
        Self { adapter, aggregator, _server: server }
    }

    /// The adapter's first unfiltered page, before any liveness filtering - every row the venue
    /// sent, so row-level checks don't depend on how many markets happened to be open.
    async fn raw_rows(&self) -> Vec<VenueMarket> {
        self.adapter.list_markets(Default::default()).await.expect("first page parses").items
    }

    async fn markets(&self) -> Vec<VenueMarket> {
        let store = self.aggregator.markets.read().await;
        let mut all = Vec::new();
        for breakdown in store.category_breakdown() {
            all.extend(
                store
                    .markets_in(&breakdown.category.as_str().into(), crate::utils::time::now_unix(), 3_650 * 86_400, None)
                    .into_iter()
                    .cloned(),
            );
        }
        all
    }
}

/// Runs `check` against every venue in `VENUES`, naming the venue on failure.
async fn for_each_venue<F, Fut>(check: F)
where
    F: Fn(&'static str, Refreshed) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    for (venue_dir, base_path, factory) in VENUES {
        let refreshed = Refreshed::new(venue_dir, base_path, *factory).await;
        check(venue_dir, refreshed).await;
    }
}

#[tokio::test]
async fn the_refresh_completes_cleanly() {
    // An empty catalog is legitimate (Panta had no bettable market at capture time); a stale or
    // unhealthy one after a clean replay is not.
    for_each_venue(|venue, r| async move {
        let statuses = r.aggregator.markets.read().await.bucket_statuses();
        assert!(statuses.iter().all(|s| !s.stale), "{venue}: a bucket is stale after a clean refresh");
        let health = r.aggregator.health.snapshot();
        assert!(health.iter().all(|h| h.last_error.is_none()), "{venue}: refresh recorded an error: {health:?}");
    })
    .await;
}

#[tokio::test]
async fn pages_parse_into_rows() {
    for_each_venue(|venue, r| async move {
        let rows = r.raw_rows().await;
        assert!(!rows.is_empty(), "{venue}: first page parsed to no rows");
        for row in &rows {
            assert!(!row.venue_market_id.is_empty(), "{venue}: a row with no id");
            assert!(!row.venue_category.trim().is_empty(), "{venue}/{}: no venue category", row.venue_market_id);
        }
    })
    .await;
}

#[tokio::test]
async fn a_venue_with_a_category_filter_lists_its_categories() {
    for_each_venue(|venue, r| async move {
        if r.adapter.capabilities().supports_category_filter {
            let categories = r.adapter.filter_categories().await.expect("categories");
            assert!(!categories.is_empty(), "{venue}: claims a category filter but lists no categories");
        }
    })
    .await;
}

#[tokio::test]
async fn ids_are_unique_and_stable_across_refreshes() {
    for_each_venue(|venue, r| async move {
        let first: Vec<String> = r.markets().await.into_iter().map(|m| m.venue_market_id).collect();
        let unique: HashSet<&String> = first.iter().collect();
        assert_eq!(unique.len(), first.len(), "{venue}: duplicate market ids");
        r.aggregator.refresh().await;
        let second: HashSet<String> = r.markets().await.into_iter().map(|m| m.venue_market_id).collect();
        assert_eq!(unique.into_iter().cloned().collect::<HashSet<_>>(), second, "{venue}: ids changed between refreshes");
    })
    .await;
}

#[tokio::test]
async fn every_market_is_normalised() {
    for_each_venue(|venue, r| async move {
        let served = r.markets().await;
        // Set at ingestion, so only served markets carry it.
        for m in &served {
            assert!(!m.display_title.trim().is_empty(), "{venue}/{}: no display title", m.venue_market_id);
        }
        for m in served.iter().chain(r.raw_rows().await.iter()) {
            let id = &m.venue_market_id;
            assert_eq!(m.venue_id, venue, "{venue}/{id}: venue_id");
            assert!(!m.venue_category.trim().is_empty(), "{venue}/{id}: no venue category");
            assert!(!m.category.is_empty(), "{venue}/{id}: no category");
            // Only for open markets: a closed one may legitimately have resolved early (a
            // price-threshold market settles when the price is hit - 991 of Jupiter's captured
            // markets did).
            if m.is_open(crate::utils::time::now_unix()) && m.close_time > 0 && m.resolution_time > 0 {
                assert!(m.close_time <= m.resolution_time, "{venue}/{id}: open, but closes after it resolves");
            }
            for price in [&m.yes_price, &m.no_price].into_iter().flatten() {
                let price: Decimal = price.parse().unwrap_or_else(|_| panic!("{venue}/{id}: unparseable price {price}"));
                assert!(price >= Decimal::ZERO && price <= Decimal::ONE, "{venue}/{id}: price {price} outside [0, 1]");
            }
        }
    })
    .await;
}

#[tokio::test]
async fn fields_the_model_does_not_name_are_kept_not_dropped() {
    for_each_venue(|venue, r| async move {
        let rows = r.raw_rows().await;
        let with_extra = rows.iter().filter(|m| !m.extra.is_empty()).count();
        assert!(with_extra > 0, "{venue}: no market kept any unmodelled field in `extra`");
    })
    .await;
}

#[tokio::test]
async fn an_unreachable_venue_is_an_error_not_an_empty_catalog() {
    for (venue_dir, _, factory) in VENUES {
        // Nothing listens on port 9.
        let adapter = factory("http://127.0.0.1:9/api".to_string());
        let result = adapter.list_markets(Default::default()).await;
        assert!(result.is_err(), "{venue_dir}: an unreachable venue returned Ok");
    }
}

#[tokio::test]
#[ignore = "diagnostic: prints what each venue produced"]
async fn print_what_each_venue_produced() {
    for_each_venue(|venue, r| async move {
        let markets = r.markets().await;
        println!("== {venue}: {} markets", markets.len());
        for b in r.aggregator.markets.read().await.category_breakdown() {
            println!("   {} ({}): {:?}", b.category, b.markets, b.venues.iter().map(|v| &v.native).collect::<Vec<_>>());
        }
        for m in markets.iter().take(6) {
            println!("   - {} | kind={:?} entity={:?} tags={:?}", m.display_title, m.market_kind, m.entity.as_ref().map(|e| &e.name), m.event.as_ref().map(|e| &e.tags));
        }
    })
    .await;
}

/// Every venue refreshed from fixtures into one store's worth of markets.
pub(crate) async fn all_fixture_markets() -> Vec<VenueMarket> {
    let mut all = Vec::new();
    for (venue_dir, base_path, factory) in VENUES {
        let refreshed = Refreshed::new(venue_dir, base_path, *factory).await;
        all.extend(refreshed.markets().await);
    }
    all
}

#[tokio::test]
#[ignore = "diagnostic: builds the event graph from every venue's fixtures and prints a summary"]
async fn print_the_event_graph_built_from_fixtures() {
    use crate::engine::events::build_events;
    let markets = all_fixture_markets().await;
    let refs: Vec<&VenueMarket> = markets.iter().collect();
    let started = std::time::Instant::now();
    let graph = build_events(&refs, |_| true, crate::utils::time::now_unix());
    println!("built {} events from {} markets in {:?}", graph.events.len(), markets.len(), started.elapsed());
    let mut reasons = std::collections::BTreeMap::new();
    for e in &graph.events {
        *reasons.entry(e.grouping.reason.clone()).or_insert(0) += 1;
    }
    println!("events by grouping reason: {reasons:?}");
    println!("cross-venue events: {}", graph.events.iter().filter(|e| e.venue_count > 1).count());
    println!("events with several markets: {}", graph.events.iter().filter(|e| e.market_count > 1).count());
    println!("events without an image: {}", graph.events.iter().filter(|e| e.image_url.is_none()).count());
    println!("== events grouped by shared words");
    for e in graph.events.iter().filter(|e| e.grouping.reason == "terms") {
        println!("* {} ({} markets; {})", e.title, e.market_count, e.grouping.detail);
        for m in e.markets.iter().take(4) {
            println!("     {} | {}", m.market.venue_id, m.market.title);
        }
    }
    println!("== lone panta markets");
    for e in graph.events.iter().filter(|e| e.grouping.reason == "single" && e.venues == ["panta"]).take(30) {
        println!("- {} | image={}", e.markets[0].market.title, e.image_url.is_some());
    }
    for e in graph.events.iter().step_by(graph.events.len().max(1) / 25) {
        println!("- [{}|{}|{}] {}  ({} markets, {})", e.category, e.domain, e.grouping.reason, e.title, e.market_count, e.grouping.detail);
        for m in e.markets.iter().take(2) {
            println!("     {} | {}", m.market.venue_id, m.market.title);
        }
    }
}

/// An aggregator holding every venue's fixtures, refreshed through the real pipeline, with the
/// clock frozen at capture time. Keep the servers alive for as long as the aggregator is used.
pub(crate) async fn fixture_aggregator() -> (Aggregator, Vec<FixtureServer>) {
    freeze_now_for_this_thread(fixtures_captured_at());
    let mut servers = Vec::new();
    let mut venues = Vec::new();
    for (venue_dir, base_path, factory) in VENUES {
        let server = FixtureServer::start(venue_dir).await;
        let adapter = factory(format!("{}{base_path}", server.base));
        let settings = VenueSettings { page_cap: 1, min_request_gap: std::time::Duration::ZERO, ..VenueSettings::default() };
        venues.push(RegisteredVenue { adapter, settings });
        servers.push(server);
    }
    let aggregator = Aggregator::new(venues, CategoryAliases::defaults(), None);
    assert!(aggregator.refresh().await);
    (aggregator, servers)
}
