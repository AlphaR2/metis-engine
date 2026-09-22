//! The composition root: wires the curated venue network, the raw market store, the canonical
//! event graph, per-venue health, and the position index together. Everything the API layer needs
//! to serve discovery and routing without knowing venue internals itself.
//!
//! `refresh()` feeds two stores from one pass over the venues:
//! - `markets` — raw `VenueMarket`s bucketed by (venue, category). This backs `GET /markets`, the
//!   primary discovery path, and is replaced wholesale each cycle.
//! - `graph` — the canonical event graph, which groups markets across venues by title similarity.
//!   Retained and still served by `/events`, but it only earns its keep once several venues quote
//!   the same real-world event.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::errors::AdapterError;
use crate::models::venue::{Category, ListParams, MarketPage, VenueMarket};
use crate::repositories::graph_repository::CanonicalGraph;
use crate::repositories::health_repository::HealthRegistry;
use crate::repositories::market_repository::MarketStore;
use crate::repositories::market_store_db;
use crate::repositories::position_repository::PositionIndex;
use crate::services::VenueAdapter;
use crate::services::matching;
use crate::utils::time::now_unix;

pub struct Aggregator {
    /// The curated venue network - one adapter per integrated venue.
    pub venues: Vec<Arc<dyn VenueAdapter>>,
    /// Raw venue markets by (venue, category) - what `GET /markets` serves.
    pub markets: tokio::sync::RwLock<MarketStore>,
    /// The canonical event/outcome graph, also fed by `refresh()`.
    pub graph: tokio::sync::RwLock<CanonicalGraph>,
    /// Live reachability state for every venue.
    pub health: HealthRegistry,
    /// Cross-venue positions recorded so far.
    pub positions: PositionIndex,
    /// Durability for the market store. `None` means in-memory only (no `DATABASE_URL`), which is
    /// a supported mode - reads never touch this, it only persists what refresh produces and
    /// repopulates the store on boot.
    db: Option<sqlx::PgPool>,
    /// Held for the duration of a refresh. A refresh fans out across every venue and category and
    /// can run for a while, so the periodic timer and a manual `POST /refresh` must not be able to
    /// stack up concurrent full passes against the venues' rate limits.
    refresh_lock: tokio::sync::Mutex<()>,
}

impl Aggregator {
    /// Wires up an aggregator for the given venues, starting all healthy.
    pub fn new(venues: Vec<Arc<dyn VenueAdapter>>, db: Option<sqlx::PgPool>) -> Self {
        let health = HealthRegistry::new(venues.iter().map(|v| v.id()));
        Self {
            venues,
            db,
            markets: tokio::sync::RwLock::new(MarketStore::new()),
            graph: tokio::sync::RwLock::new(CanonicalGraph::new()),
            health,
            positions: PositionIndex::new(),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Pages fetched per (venue, category) chain before stopping.
    ///
    /// Neither venue can answer "what closes soonest" server-side - Jupiter's `sortBy` accepts
    /// only `volume|volume24hr|beginAt`, and Panta ignores sort params entirely - so a close-time
    /// window cannot be pushed down and there is no page at which we can prove nothing relevant
    /// remains. Confirmed against Jupiter's sports catalog: markets closing within 7 days appear
    /// at every depth sampled (95, 88, 81, 32, 35 at offsets 0, 50, 100, 200, 400).
    ///
    /// So each chain instead takes the venue's most-traded slice first (see the per-client sort)
    /// and caps depth here. The cap is a deliberate product call - the busiest N markets per
    /// category - not an attempt at completeness.
    const MAX_PAGES_PER_CATEGORY: usize = 10;

    /// Category chains allowed to be in flight against a single venue at once.
    ///
    /// One. Jupiter returns 429 well below the rate a parallel fan-out produces - three
    /// concurrent chains still tripped it - and a 429 is indistinguishable from a real outage to
    /// `HealthRegistry`, so an over-eager refresh can mark a perfectly healthy venue unroutable
    /// purely by hammering it. Venues still refresh concurrently with *each other*; this only
    /// serializes the chains within one venue.
    const MAX_CONCURRENT_CHAINS_PER_VENUE: usize = 1;

    /// Minimum gap between consecutive page requests to the same venue. Paired with the
    /// concurrency limit above: the limit stops parallel bursts, this paces the serial ones.
    const MIN_REQUEST_GAP: Duration = Duration::from_millis(250);

    /// Whether a refresh is running right now - lets the API answer honestly instead of
    /// accepting a trigger that will be dropped.
    pub fn refresh_in_progress(&self) -> bool {
        self.refresh_lock.try_lock().is_err()
    }

    /// Runs a full refresh, unless one is already running - in which case this returns immediately
    /// rather than queueing a second fan-out behind it. Returns whether it actually ran.
    pub async fn refresh(&self) -> bool {
        let Ok(_guard) = self.refresh_lock.try_lock() else {
            tracing::info!("refresh already in progress - skipping this trigger");
            return false;
        };
        let started = std::time::Instant::now();
        futures::future::join_all(self.venues.iter().map(|venue| self.refresh_venue(venue))).await;
        let held = self.markets.read().await.len();
        tracing::info!("refresh complete in {:?} - {held} markets held", started.elapsed());
        true
    }

    /// Refreshes one venue across every category it supports, concurrently.
    ///
    /// Both integrated venues honour a server-side `category` filter, but on *different*
    /// vocabularies, so the canonical category is translated per venue and the ones a venue has no
    /// equivalent for are skipped rather than sent and rejected. A venue with no category filter
    /// at all gets one chain whose results are bucketed client-side by each market's own category.
    async fn refresh_venue(&self, venue: &Arc<dyn VenueAdapter>) {
        let supported: Vec<(Category, String)> = Category::all()
            .into_iter()
            .filter_map(|c| venue.native_category(&c).map(|native| (c, native)))
            .collect();

        let total: usize = if supported.is_empty() {
            self.refresh_uncategorized(venue).await
        } else {
            let permits = tokio::sync::Semaphore::new(Self::MAX_CONCURRENT_CHAINS_PER_VENUE);
            futures::future::join_all(supported.iter().map(|(canonical, _)| {
                let permits = &permits;
                async move {
                    let _permit = permits.acquire().await.expect("semaphore is never closed");
                    self.refresh_category(venue, canonical).await
                }
            }))
            .await
            .into_iter()
            .sum()
        };

        crate::metrics::VENUE_MARKETS_SEEN.with_label_values(&[venue.id()]).set(total as i64);
    }

    /// One venue, one category: walk the pages, keep what's bettable, replace that bucket.
    async fn refresh_category(&self, venue: &Arc<dyn VenueAdapter>, category: &Category) -> usize {
        let label = format!("{}[{}]", venue.id(), String::from(category.clone()));
        let markets = self.collect_pages(venue, Some(category.clone()), &label).await;
        let count = markets.len();

        self.attach_to_event_graph(venue, &markets).await;
        self.persist_bucket(venue.id(), category, &markets).await;
        self.markets.write().await.replace_bucket(venue.id(), category, markets, now_unix());

        tracing::info!("{label}: {count} bettable markets");
        count
    }

    /// A venue with no server-side category filter: one chain, then bucket by each market's own
    /// category so `GET /markets?category=` still works for it.
    async fn refresh_uncategorized(&self, venue: &Arc<dyn VenueAdapter>) -> usize {
        let label = venue.id().to_string();
        let markets = self.collect_pages(venue, None, &label).await;
        let count = markets.len();

        self.attach_to_event_graph(venue, &markets).await;

        let mut by_category: HashMap<String, Vec<VenueMarket>> = HashMap::new();
        for market in markets {
            by_category.entry(String::from(market.category.clone())).or_default().push(market);
        }
        let now = now_unix();
        for (category, bucket) in by_category {
            let category = Category::from(category);
            self.persist_bucket(venue.id(), &category, &bucket).await;
            self.markets.write().await.replace_bucket(venue.id(), &category, bucket, now);
        }

        tracing::info!("{label}: {count} bettable markets");
        count
    }

    /// Writes one bucket through to Postgres, if configured.
    ///
    /// A persistence failure is logged and swallowed rather than failing the refresh: the
    /// in-memory store is the read path, so the catalog stays correct and servable even when the
    /// database is unreachable. The cost is only that a restart during an outage loses freshness.
    async fn persist_bucket(&self, venue_id: &str, category: &Category, markets: &[VenueMarket]) {
        let Some(pool) = &self.db else { return };
        if let Err(err) = market_store_db::replace_bucket(pool, venue_id, category, markets).await {
            tracing::warn!("{venue_id}[{}]: could not persist markets: {err}", String::from(category.clone()));
        }
    }

    /// Repopulates the in-memory store from Postgres at boot, so a restart serves the previous
    /// catalog immediately instead of an empty list until the first refresh lands. Returns how
    /// many markets were restored.
    pub async fn warm_start(&self) -> usize {
        let Some(pool) = &self.db else { return 0 };
        match market_store_db::load_all(pool).await {
            Ok(buckets) => {
                let now = now_unix();
                let mut store = self.markets.write().await;
                let mut restored = 0;
                for (venue_id, category, markets) in buckets {
                    restored += markets.len();
                    store.replace_bucket(&venue_id, &category, markets, now);
                }
                restored
            }
            Err(err) => {
                tracing::warn!("could not warm-start markets from Postgres: {err}");
                0
            }
        }
    }

    /// Feeds the canonical event graph. Kept alongside the market store so `/events` keeps
    /// working; it is not on the `GET /markets` path.
    async fn attach_to_event_graph(&self, venue: &Arc<dyn VenueAdapter>, markets: &[VenueMarket]) {
        let framing = venue.capabilities().framing;
        let mut graph = self.graph.write().await;
        for market in markets {
            matching::attach_market(&mut graph, venue.id(), framing, market.clone());
        }
    }

    /// Walks one pagination chain to its end (or the page cap), returning every market that passes
    /// `is_bettable`. Dead, untitled and past-close rows are dropped here so nothing downstream
    /// has to re-check them.
    async fn collect_pages(
        &self,
        venue: &Arc<dyn VenueAdapter>,
        category: Option<Category>,
        label: &str,
    ) -> Vec<VenueMarket> {
        let now = now_unix();
        let mut collected: Vec<VenueMarket> = Vec::new();
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut cursor: Option<String> = None;

        for page_number in 0..Self::MAX_PAGES_PER_CATEGORY {
            let params = ListParams { cursor: cursor.clone(), category: category.clone(), ..ListParams::default() };

            // Pace the chain so a fast venue doesn't get a burst of back-to-back requests.
            if page_number > 0 {
                tokio::time::sleep(Self::MIN_REQUEST_GAP).await;
            }

            let page = match Self::list_page_with_retry(venue, params, label).await {
                Ok(page) => page,
                Err(err) => {
                    tracing::warn!("{label}: page {} failed: {err}", page_number + 1);
                    self.health.record_failure(venue.id(), err.to_string());
                    break;
                }
            };
            self.health.record_success(venue.id());

            // Guard against a venue that hands back rows we already have. Panta does exactly this:
            // its `nextCursor` is a constant, so following it re-serves page 1 forever - six
            // requests returned 300 rows containing only 50 distinct markets. Deduplicating by id
            // makes that harmless, and the cursor check below stops us paying for it at all.
            let mut new_on_page = 0usize;
            for market in page.items {
                if market.is_bettable(now) && seen_ids.insert(market.venue_market_id.clone()) {
                    collected.push(market);
                    new_on_page += 1;
                }
            }

            if page_number == 0 || (page_number + 1) % 5 == 0 {
                tracing::debug!(
                    "{label}: page {} -> {new_on_page} new bettable (total {})",
                    page_number + 1,
                    collected.len()
                );
            }

            match page.next_cursor {
                // A cursor identical to the one we just used means pagination isn't advancing.
                // Treat it as the end of the chain rather than looping to the page cap.
                Some(next) if Some(&next) == cursor.as_ref() => {
                    tracing::warn!("{label}: venue returned an unchanged cursor - pagination is not advancing, stopping");
                    break;
                }
                Some(next) => cursor = Some(next),
                None => break,
            }

            if page_number + 1 == Self::MAX_PAGES_PER_CATEGORY {
                tracing::info!("{label}: reached the {}-page cap", Self::MAX_PAGES_PER_CATEGORY);
            }
        }

        collected
    }

    /// Fetches one page, retrying transient failures (timeouts, dropped bodies) with a short
    /// backoff so one slow response doesn't end a whole chain.
    async fn list_page_with_retry(
        venue: &Arc<dyn VenueAdapter>,
        params: ListParams,
        label: &str,
    ) -> Result<MarketPage, AdapterError> {
        const ATTEMPTS: u32 = 3;
        let mut attempt = 1;
        loop {
            match venue.list_markets(params.clone()).await {
                Ok(page) => return Ok(page),
                // An auth rejection is final - three attempts at a bad key just wastes seconds
                // per page and buries the real cause in retry noise.
                Err(err) if err.is_auth_failure() => {
                    tracing::error!(
                        "{label}: {err} - check this venue's API key; skipping the rest of this chain"
                    );
                    return Err(err);
                }
                Err(err) if attempt < ATTEMPTS => {
                    // A 429 means "slow down", not "something broke" - waiting 500ms and trying
                    // again just earns another one, so back off substantially further for it.
                    let backoff = if err.is_rate_limited() {
                        Duration::from_secs(2 * u64::from(attempt))
                    } else {
                        Duration::from_millis(500 * u64::from(attempt))
                    };
                    tracing::warn!("{label}: page fetch failed (attempt {attempt}/{ATTEMPTS}): {err} - retrying in {backoff:?}");
                    tokio::time::sleep(backoff).await;
                    attempt += 1;
                }
                Err(err) => return Err(err),
            }
        }
    }
}
