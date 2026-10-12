//! The composition root: wires the venue registry, the raw market store, the canonical event
//! graph, per-venue health and the live-quote cache together. Everything the API layer needs to
//! serve discovery and routing without knowing venue internals itself.
//!
//! `refresh()` feeds the market store from one pass over the venues, then rebuilds the event graph
//! from it:
//! - `markets` — raw `VenueMarket`s bucketed by (venue, category). The source the graph is built
//!   from, and what is persisted.
//! - `events` — the canonical event graph (`engine`): events, questions, outcomes and rated
//!   listings, matched across venues. Backs `/v1`. Built fresh and swapped in whole each time,
//!   so reads never wait on a rebuild and nothing dead accumulates.
//!
//! Nothing here knows a venue by name. What differs between venues is either the adapter's job or
//! a `VenueSettings` value from config.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use crate::engine::ai::{self, Cached};
use crate::engine::events;
use crate::engine::graph::EventGraph;
use crate::errors::AdapterError;
use crate::models::venue::{Category, CategoryAliases, ListParams, MarketPage, VenueMarket};
use crate::repositories::health_repository::HealthRegistry;
use crate::repositories::market_repository::MarketStore;
use crate::repositories::market_store_db;
use crate::repositories::positions_cache::PositionsCache;
use crate::services::VenueAdapter;
use crate::services::llm::{self, LlmClient};
use crate::services::registry::{RegisteredVenue, VenueSettings};
use crate::utils::time::now_unix;

pub struct Aggregator {
    /// The venue network - one adapter per enabled venue.
    pub venues: Vec<Arc<dyn VenueAdapter>>,
    /// Per-venue pacing and limits, by venue id.
    settings: HashMap<&'static str, VenueSettings>,
    /// Maps each venue's category names onto shared names.
    aliases: CategoryAliases,
    /// Raw venue markets by (venue, category) - the source the event graph is built from.
    pub markets: tokio::sync::RwLock<MarketStore>,
    /// The canonical event graph behind `/v1`, swapped in whole after every rebuild.
    pub events: tokio::sync::RwLock<Arc<EventGraph>>,
    /// A model's decisions about lone markets, by `venue:market_id`, kept so each is asked once.
    hints: tokio::sync::RwLock<HashMap<String, Cached>>,
    /// The model that assists event grouping. `None` (no `ANTHROPIC_API_KEY`) means rules only.
    llm: Option<LlmClient>,
    /// Live reachability state for every venue.
    pub health: HealthRegistry,
    /// Seconds-long cache of complete wallet position reads.
    pub positions: PositionsCache,
    /// Durability for the market store. `None` means in-memory only (no `DATABASE_URL`), which is
    /// a supported mode - reads never touch this, it only persists what refresh produces and
    /// repopulates the store on boot.
    db: Option<sqlx::PgPool>,
    /// Held for the duration of a refresh. A refresh fans out across every venue and category and
    /// can run for a while, so the periodic timer and a manual `POST /refresh` must not be able to
    /// stack up concurrent full passes against the venues' rate limits.
    refresh_lock: tokio::sync::Mutex<()>,
}

/// How many lone markets one refresh may put to the model, and how many go in one request.
const MAX_AI_MARKETS_PER_REFRESH: usize = 200;
const AI_BATCH: usize = 25;

/// Why one pagination chain produced nothing usable.
enum ChainFailure {
    /// The venue rejected the category filter itself (a 4xx on the first page). Not an outage:
    /// that category is skipped this pass and its markets still arrive via the unfiltered sweep.
    FilterRejected(AdapterError),
    /// Anything else. The venue's whole refresh is abandoned and its previous catalog kept.
    Failed(AdapterError),
}

impl Aggregator {
    /// Wires up an aggregator for the given venues, starting all healthy.
    pub fn new(venues: Vec<RegisteredVenue>, aliases: CategoryAliases, db: Option<sqlx::PgPool>) -> Self {
        let health = HealthRegistry::new(venues.iter().map(|v| v.adapter.id()));
        let settings = venues.iter().map(|v| (v.adapter.id(), v.settings.clone())).collect();
        Self {
            venues: venues.into_iter().map(|v| v.adapter).collect(),
            settings,
            aliases,
            db,
            markets: tokio::sync::RwLock::new(MarketStore::new()),
            events: tokio::sync::RwLock::new(Arc::new(EventGraph::default())),
            hints: tokio::sync::RwLock::new(HashMap::new()),
            llm: LlmClient::from_env(),
            health,
            positions: PositionsCache::new(),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The enabled venue with this id.
    pub fn venue(&self, venue_id: &str) -> Option<Arc<dyn VenueAdapter>> {
        self.venues.iter().find(|v| v.id() == venue_id).cloned()
    }

    fn settings_for(&self, venue_id: &str) -> VenueSettings {
        self.settings.get(venue_id).cloned().unwrap_or_default()
    }

    /// Whether a refresh is running right now - lets the API answer honestly instead of
    /// accepting a trigger that will be dropped.
    pub fn refresh_in_progress(&self) -> bool {
        self.refresh_lock.try_lock().is_err()
    }

    /// Runs a full refresh, unless one is already running - in which case this returns immediately
    /// rather than queueing a second fan-out behind it. Returns whether it actually ran.
    ///
    /// Venues refresh concurrently and independently: one slow or failing venue never delays or
    /// affects another's catalog.
    pub async fn refresh(&self) -> bool {
        let Ok(_guard) = self.refresh_lock.try_lock() else {
            tracing::info!("refresh already in progress - skipping this trigger");
            return false;
        };
        let started = std::time::Instant::now();
        futures::future::join_all(self.venues.iter().map(|venue| self.refresh_venue(venue))).await;
        self.rebuild_graphs().await;
        let held = self.markets.read().await.len();
        crate::metrics::REFRESH_DURATION.observe(started.elapsed().as_secs_f64());
        tracing::info!("refresh complete in {:?} - {held} markets held", started.elapsed());
        true
    }

    /// A refresh that, if it leaves nothing to serve, is tried again soon instead of after a whole
    /// refresh interval.
    ///
    /// The boot refresh runs on whatever network the machine has at that moment. If it is down for a
    /// minute every venue fails, nothing is held (and no database to warm-start from), and the next
    /// scheduled pass is a quarter of an hour away - an empty catalog and a 503 `/ready` for that
    /// whole time. Retrying every `retry_after`, up to `max_retries` times, bridges that gap. Once
    /// anything is held it stops: a partial catalog is the regular schedule's to complete.
    pub async fn refresh_until_held(&self, retry_after: Duration, max_retries: u32) {
        self.refresh().await;
        for attempt in 1..=max_retries {
            if self.markets.read().await.len() > 0 {
                return;
            }
            tracing::warn!("no markets held after the refresh - trying again in {retry_after:?} (retry {attempt}/{max_retries})");
            tokio::time::sleep(retry_after).await;
            self.refresh().await;
        }
    }

    /// Refreshes one venue: every category its filter accepts, plus one unfiltered sweep.
    ///
    /// The sweep is how categories the venue doesn't advertise are found - Panta's own
    /// `/categories/` lists 8 names while its markets report 11 (`docs/venue-fields.md`). Every
    /// market is bucketed by the category *it* reports, not by the chain that fetched it, so the
    /// two views agree.
    ///
    /// All or nothing: the venue's buckets are replaced only when every chain completed. Otherwise
    /// the previous catalog is kept and marked stale - a failed refresh is not evidence that
    /// markets disappeared.
    async fn refresh_venue(&self, venue: &Arc<dyn VenueAdapter>) {
        let venue_id = venue.id();
        let settings = self.settings_for(venue_id);

        if self.health.breaker_open(venue_id, now_unix(), settings.breaker_cooldown) {
            let kept = self.markets.write().await.mark_venue_stale(venue_id);
            // The breaker spares a venue that keeps timing out and keeps its last good catalog. With
            // nothing held there is nothing to keep, and skipping would only leave the venue empty
            // for the whole cooldown - as after a boot on a bad network - so it is tried anyway.
            if kept > 0 {
                tracing::warn!("{venue_id}: circuit open after repeated failures - skipping this pass, keeping {kept} markets");
                return;
            }
        }

        let mut chains: Vec<Option<String>> = vec![None];
        match venue.filter_categories().await {
            Ok(categories) => chains.extend(categories.into_iter().map(Some)),
            Err(err) => {
                // Fall back to the categories this venue reported last time, so a flaky
                // categories endpoint costs freshness, not coverage.
                self.health.record_error(venue_id, &err);
                let known = self.markets.read().await.venue_categories_held(venue_id);
                tracing::warn!("{venue_id}: could not list categories ({err}) - using the {} seen last pass", known.len());
                chains.extend(known.into_iter().map(Some));
            }
        }
        let mut seen_chains = HashSet::new();
        chains.retain(|c| seen_chains.insert(c.clone()));

        let permits = tokio::sync::Semaphore::new(settings.max_concurrent_chains.max(1));
        let results = futures::future::join_all(chains.iter().map(|category| {
            let permits = &permits;
            let settings = &settings;
            async move {
                let _permit = permits.acquire().await.expect("semaphore is never closed");
                let label = match category {
                    Some(c) => format!("{venue_id}[{c}]"),
                    None => format!("{venue_id}[*]"),
                };
                (label.clone(), self.collect_pages(venue, category.clone(), &label, settings).await)
            }
        }))
        .await;

        let mut merged: Vec<VenueMarket> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();
        for (label, result) in results {
            match result {
                Ok(markets) => {
                    for market in markets {
                        if seen_ids.insert(market.venue_market_id.clone()) {
                            merged.push(market);
                        }
                    }
                }
                Err(ChainFailure::FilterRejected(err)) => {
                    tracing::warn!("{label}: venue rejected this category filter ({err}) - skipped; its markets still arrive via the unfiltered sweep");
                }
                Err(ChainFailure::Failed(err)) => {
                    let kept = self.markets.write().await.mark_venue_stale(venue_id);
                    tracing::warn!("{label}: refresh incomplete ({err}) - keeping the previous {kept} {venue_id} markets, marked stale");
                    crate::metrics::VENUE_MARKETS_SEEN.with_label_values(&[venue_id]).set(kept as i64);
                    return;
                }
            }
        }

        for market in &mut merged {
            self.normalize(venue_id, market);
        }
        let count = merged.len();

        let mut by_category: HashMap<Category, Vec<VenueMarket>> = HashMap::new();
        for market in merged {
            by_category.entry(market.category.clone()).or_default().push(market);
        }
        let by_category: Vec<(Category, Vec<VenueMarket>)> = by_category.into_iter().collect();
        self.persist_venue(venue_id, &by_category).await;
        let categories = by_category.len();
        self.markets.write().await.replace_venue(venue_id, by_category, now_unix());

        crate::metrics::VENUE_MARKETS_SEEN.with_label_values(&[venue_id]).set(count as i64);
        tracing::info!("{venue_id}: {count} bettable markets across {categories} categories");
    }

    /// The venue-independent fix-ups every market gets on the way in: its venue id, its category
    /// kept verbatim and mapped to the shared name, and a title that reads on its own.
    fn normalize(&self, venue_id: &'static str, market: &mut VenueMarket) {
        market.venue_id = venue_id.to_string();
        if market.venue_category.trim().is_empty() {
            market.venue_category = market.category.as_str().to_string();
        }
        market.category = self.aliases.canonical(&market.venue_category);
        market.display_title = market.compose_display_title();
    }

    /// Writes a venue's whole catalog through to Postgres, if configured.
    ///
    /// A persistence failure is logged and swallowed rather than failing the refresh: the
    /// in-memory store is the read path, so the catalog stays correct and servable even when the
    /// database is unreachable. The cost is only that a restart during an outage loses freshness.
    async fn persist_venue(&self, venue_id: &str, by_category: &[(Category, Vec<VenueMarket>)]) {
        let Some(pool) = &self.db else { return };
        if let Err(err) = market_store_db::replace_venue(pool, venue_id, by_category).await {
            tracing::warn!("{venue_id}: could not persist markets: {err}");
        }
    }

    /// Repopulates the in-memory store from Postgres at boot, so a restart serves the previous
    /// catalog immediately instead of an empty list until the first refresh lands. Returns how
    /// many markets were restored.
    pub async fn warm_start(&self) -> usize {
        let Some(pool) = &self.db else { return 0 };
        match crate::repositories::subjects_db::load(pool).await {
            Ok(saved) => *self.hints.write().await = saved,
            Err(err) => tracing::warn!("could not load the model's grouping decisions from Postgres: {err}"),
        }
        match market_store_db::load_all(pool).await {
            Ok(buckets) => {
                let now = now_unix();
                let mut restored = 0;
                {
                    let mut store = self.markets.write().await;
                    // Warm start runs in the background, so an operator refresh can land first. A
                    // venue that already has live buckets keeps them; the stored copy is older.
                    let live: std::collections::HashSet<String> =
                        store.bucket_statuses().into_iter().map(|b| b.venue_id).collect();
                    for (venue_id, category, markets) in buckets {
                        if live.contains(&venue_id) {
                            continue;
                        }
                        restored += markets.len();
                        store.replace_bucket(&venue_id, &category, markets, now);
                    }
                }
                if restored > 0 {
                    self.rebuild_graphs().await;
                }
                restored
            }
            Err(err) => {
                tracing::warn!("could not warm-start markets from Postgres: {err}");
                0
            }
        }
    }

    /// Rebuilds the event graph from the market store and swaps it in. Building is CPU work over
    /// the whole catalog, so it runs on a blocking thread, and the result replaces the old graph in
    /// one write: readers see either the old graph or the new one, never a partial one.
    ///
    /// The graph is published from the rules (plus any earlier model decisions) first. If a model is
    /// configured, the lone markets that resemble something are then put to it, and the graph is
    /// rebuilt once with its answers. A model that is down or slow costs nothing but that rebuild.
    pub async fn rebuild_graphs(&self) {
        let started = std::time::Instant::now();
        let markets = Arc::new(self.markets.read().await.all_markets());
        let cached = self.hints.read().await.clone();
        let now = now_unix();
        let routable: HashSet<String> =
            self.venues.iter().map(|v| v.id()).filter(|id| self.health.is_routable(id)).map(str::to_string).collect();

        let work = {
            let (markets, cached, routable) = (markets.clone(), cached.clone(), routable.clone());
            tokio::task::spawn_blocking(move || {
                let refs: Vec<&VenueMarket> = markets.iter().collect();
                let prepared = events::prepare(&refs, now);
                let clusters = prepared.clusters_with(&cached);
                let input = prepared.ai_input(&clusters, &cached, MAX_AI_MARKETS_PER_REFRESH);
                (prepared.build_with(clusters, |venue| routable.contains(venue), now), input)
            })
            .await
        };
        let (graph, input) = match work {
            Ok(done) => done,
            Err(err) => {
                tracing::error!("event graph rebuild panicked - keeping the previous graph: {err}");
                return;
            }
        };
        let (events_count, multi) = (graph.events.len(), graph.events.iter().filter(|e| e.market_count > 1).count());
        let without_image = graph.events.iter().filter(|e| e.image_url.is_none()).count();
        *self.events.write().await = Arc::new(graph);
        tracing::info!(
            "event graph rebuilt in {:?}: {events_count} events ({multi} with several markets), {without_image} without an image",
            started.elapsed()
        );

        if input.markets.is_empty() {
            return;
        }
        let Some(llm) = &self.llm else { return };
        let learned = self.ask_model(llm, input).await;
        if learned == 0 {
            return;
        }
        // Rebuild once with what the model said. No further model calls: they are all cached now.
        let cached = self.hints.read().await.clone();
        let rebuilt = tokio::task::spawn_blocking(move || {
            let refs: Vec<&VenueMarket> = markets.iter().collect();
            let prepared = events::prepare(&refs, now);
            let clusters = prepared.clusters_with(&cached);
            prepared.build_with(clusters, |venue| routable.contains(venue), now)
        })
        .await;
        match rebuilt {
            Ok(graph) => {
                tracing::info!("event graph rebuilt with {learned} model decisions: {} events", graph.events.len());
                *self.events.write().await = Arc::new(graph);
            }
            Err(err) => tracing::error!("event graph rebuild with model decisions panicked - keeping the rules-only graph: {err}"),
        }
    }

    /// Puts `input` to the model in batches and caches what it says. Returns how many decisions
    /// were learned. A failed batch is skipped; its markets are asked again next refresh.
    async fn ask_model(&self, llm: &LlmClient, input: events::AiInput) -> usize {
        let mut learned: Vec<(String, Cached)> = Vec::new();
        for batch in input.markets.chunks(AI_BATCH) {
            let ids: Vec<&str> = batch.iter().map(|m| m.id.as_str()).collect();
            let batch_id = crate::engine::graph::stable_id("b", &ids.join("|"))[2..].to_string();
            let wanted: HashSet<&str> = batch.iter().flat_map(|m| m.candidates.iter().map(String::as_str)).collect();
            let clusters: Vec<ai::AiCluster> = input.clusters.iter().filter(|c| wanted.contains(c.key.as_str())).cloned().collect();
            let prompt = ai::build_prompt(batch, &clusters);
            match llm.complete(ai::SYSTEM_PROMPT, &prompt).await {
                Ok(reply) => {
                    let valid_markets: HashSet<String> = batch.iter().map(|m| m.id.clone()).collect();
                    let valid_clusters: HashSet<String> = clusters.iter().map(|c| c.key.clone()).collect();
                    let hints = ai::parse_response(&reply, &batch_id, &valid_markets, &valid_clusters);
                    for market in batch {
                        // Every market in an answered batch is cached, including "leave alone", so it is
                        // not asked about again until its text changes.
                        let hint = hints.get(&market.id).cloned().unwrap_or_default();
                        if let Some(hash) = input.hashes.get(&market.id) {
                            learned.push((market.id.clone(), Cached::from_hint(hash.clone(), &hint)));
                        }
                    }
                }
                Err(err) => tracing::warn!("model grouping batch skipped: {err}"),
            }
        }
        if learned.is_empty() {
            return 0;
        }
        // Only an answer that changes something is worth a rebuild.
        let effective = learned.iter().filter(|(_, c)| c.join.is_some() || c.group.is_some()).count();
        if let Some(pool) = &self.db
            && let Err(err) = crate::repositories::subjects_db::save(pool, &learned, llm::MODEL).await
        {
            tracing::warn!("could not persist the model's grouping decisions: {err}");
        }
        let mut cache = self.hints.write().await;
        cache.extend(learned);
        effective
    }

    /// Walks one pagination chain to its end (or the page cap), returning every market that passes
    /// `is_bettable`. Dead, untitled and past-close rows are dropped here so nothing downstream
    /// has to re-check them.
    ///
    /// All or nothing: if any page fails (after retries) the whole chain is an error, never a
    /// silently truncated list. Reaching the page cap or a non-advancing cursor is a *complete*
    /// chain - both are deliberate stopping points, not failures.
    async fn collect_pages(
        &self,
        venue: &Arc<dyn VenueAdapter>,
        category: Option<String>,
        label: &str,
        settings: &VenueSettings,
    ) -> Result<Vec<VenueMarket>, ChainFailure> {
        let now = now_unix();
        let mut collected: Vec<VenueMarket> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut cursor: Option<String> = None;

        for page_number in 0..settings.page_cap {
            let params = ListParams { cursor: cursor.clone(), category: category.clone(), ..ListParams::default() };

            // Pace the chain so a fast venue doesn't get a burst of back-to-back requests.
            if page_number > 0 {
                tokio::time::sleep(settings.min_request_gap).await;
            }

            let page = match Self::list_page_with_retry(venue, params, label).await {
                Ok(page) => page,
                Err(err) if page_number == 0 && category.is_some() && err.is_client_rejection() => {
                    return Err(ChainFailure::FilterRejected(err));
                }
                Err(err) => {
                    tracing::warn!("{label}: page {} failed: {err}", page_number + 1);
                    self.health.record_error(venue.id(), &err);
                    return Err(ChainFailure::Failed(err));
                }
            };
            self.health.record_success(venue.id());

            // Guard against a venue that hands back rows we already have. Panta did exactly this
            // until fixed venue-side on 2026-09-28: its `nextCursor` was a constant, so following
            // it re-served page 1 forever - six requests returned 300 rows containing only 50
            // distinct markets. Deduplicating by id makes that harmless regardless of which venue
            // it happens to, and the cursor check below stops us paying for it at all.
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
                    tracing::debug!("{label}: venue returned an unchanged cursor - pagination is not advancing, stopping");
                    break;
                }
                Some(next) => cursor = Some(next),
                None => break,
            }

            if page_number + 1 == settings.page_cap {
                tracing::info!("{label}: reached the {}-page cap", settings.page_cap);
            }
        }

        Ok(collected)
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
                // An auth rejection or a rejected request is final - retrying can't fix either,
                // and three attempts just bury the real cause in retry noise.
                Err(err) if err.is_auth_failure() => {
                    tracing::error!(
                        "{label}: {err} - check this venue's API key; skipping the rest of this chain"
                    );
                    return Err(err);
                }
                Err(err) if err.is_client_rejection() => return Err(err),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ScriptedVenue;

    fn held_ids(aggregator: &Aggregator) -> Vec<String> {
        let store = aggregator.markets.try_read().unwrap();
        let mut ids: Vec<String> = store
            .markets_in(&Category::from("sports"), now_unix(), 7 * 86_400, None)
            .into_iter()
            .map(|m| m.venue_market_id.clone())
            .collect();
        ids.sort();
        ids
    }

    async fn setup(categorized: bool) -> (Arc<ScriptedVenue>, Aggregator) {
        let venue = Arc::new(ScriptedVenue::new(categorized, 5));
        let aggregator = Aggregator::new(vec![venue.registered()], CategoryAliases::defaults(), None);
        assert!(aggregator.refresh().await);
        assert_eq!(held_ids(&aggregator).len(), 15);
        venue.next_generation();
        (venue, aggregator)
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_on_the_first_page_keeps_the_previous_bucket() {
        let (venue, aggregator) = setup(true).await;
        let before = held_ids(&aggregator);
        venue.fail_pages(&[0]);
        aggregator.refresh().await;
        assert_eq!(held_ids(&aggregator), before);
        assert!(aggregator.markets.read().await.bucket_statuses()[0].stale);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_part_way_through_never_truncates_the_bucket() {
        let (venue, aggregator) = setup(true).await;
        let before = held_ids(&aggregator);
        venue.fail_pages(&[3]);
        aggregator.refresh().await;
        assert_eq!(held_ids(&aggregator), before, "pages 1-3 of the new chain must not replace the old bucket");
    }

    #[tokio::test(start_paused = true)]
    async fn a_complete_refresh_after_a_failure_replaces_the_bucket_and_clears_staleness() {
        let (venue, aggregator) = setup(true).await;
        venue.fail_pages(&[3]);
        aggregator.refresh().await;
        venue.fail_pages(&[]);
        aggregator.refresh().await;
        let ids = held_ids(&aggregator);
        assert_eq!(ids.len(), 15);
        assert!(ids.iter().all(|id| id.starts_with("g2-")));
        assert!(!aggregator.markets.read().await.bucket_statuses()[0].stale);
    }

    #[tokio::test(start_paused = true)]
    async fn the_unfiltered_sweep_finds_categories_the_venue_does_not_advertise() {
        let venue = Arc::new(ScriptedVenue::new(true, 1));
        *venue.sweep_only_category.lock().unwrap() = Some("Climate & Science".to_string());
        let aggregator = Aggregator::new(vec![venue.registered()], CategoryAliases::defaults(), None);
        aggregator.refresh().await;

        let store = aggregator.markets.read().await;
        let science = store.markets_in(&Category::from("science"), now_unix(), 7 * 86_400, None);
        assert_eq!(science.len(), 1, "aliased onto the shared name");
        assert_eq!(science[0].venue_category, "Climate & Science", "the venue's own name is kept verbatim");
        let breakdown = store.category_breakdown();
        assert_eq!(breakdown.iter().map(|b| b.category.as_str()).collect::<Vec<_>>(), ["science", "sports"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_category_filter_the_venue_rejects_does_not_stall_its_refresh() {
        let venue = Arc::new(ScriptedVenue::new(true, 1));
        *venue.filters.lock().unwrap() = vec!["sports".to_string(), "unfilterable".to_string()];
        *venue.rejected_filters.lock().unwrap() = vec!["unfilterable".to_string()];
        let aggregator = Aggregator::new(vec![venue.registered()], CategoryAliases::defaults(), None);
        aggregator.refresh().await;

        let statuses = aggregator.markets.read().await.bucket_statuses();
        assert_eq!(statuses.len(), 1);
        assert!(!statuses[0].stale);
        assert!(aggregator.health.is_routable("scripted"), "a rejected filter is not an outage");
    }

    #[tokio::test(start_paused = true)]
    async fn markets_get_a_display_title_that_reads_on_its_own() {
        let (_, aggregator) = setup(true).await;
        let store = aggregator.markets.read().await;
        let market = &store.markets_in(&Category::from("sports"), now_unix(), 7 * 86_400, None)[0];
        assert_eq!(market.display_title, market.title, "no parent event, so the title stands alone");
    }

    #[tokio::test(start_paused = true)]
    async fn a_repeatedly_failing_venue_is_skipped_until_its_cooldown_passes() {
        let (venue, aggregator) = setup(true).await;
        venue.fail_pages(&[0]);
        // Each pass runs two chains (the sweep and `sports`), and each failed chain adds one to
        // the venue's failure streak, so two failed passes open the breaker.
        aggregator.refresh().await;
        aggregator.refresh().await;
        assert!(!aggregator.health.is_routable("scripted"));
        let calls_before = *venue.list_calls.lock().unwrap();
        aggregator.refresh().await;
        assert_eq!(*venue.list_calls.lock().unwrap(), calls_before, "breaker open - no calls while cooling down");
        assert_eq!(held_ids(&aggregator).len(), 15, "the last good catalog is still served");
    }

    #[tokio::test(start_paused = true)]
    async fn an_uncategorized_venue_keeps_its_catalog_when_a_page_fails() {
        let (venue, aggregator) = setup(false).await;
        let before = held_ids(&aggregator);
        venue.fail_pages(&[2]);
        aggregator.refresh().await;
        assert_eq!(held_ids(&aggregator), before);
    }

    #[tokio::test(start_paused = true)]
    async fn an_empty_catalog_after_the_boot_refresh_is_retried_soon() {
        let venue = Arc::new(ScriptedVenue::new(true, 5));
        let aggregator = Aggregator::new(vec![venue.registered()], CategoryAliases::defaults(), None);
        venue.fail_pages(&[0]);
        aggregator.refresh().await;
        let one_pass = *venue.list_calls.lock().unwrap();
        assert_eq!(aggregator.markets.read().await.len(), 0);

        aggregator.refresh_until_held(Duration::from_secs(60), 2).await;
        let total = *venue.list_calls.lock().unwrap();
        // The setup pass, then refresh_until_held's own pass and its two retries - the breaker, which
        // opened after the first failures, does not stop a venue that holds nothing.
        assert_eq!(total, one_pass * 4, "the pass ran again twice more while nothing was held");
    }

    #[tokio::test(start_paused = true)]
    async fn a_catalog_that_is_held_is_not_retried() {
        let venue = Arc::new(ScriptedVenue::new(true, 5));
        let aggregator = Aggregator::new(vec![venue.registered()], CategoryAliases::defaults(), None);
        aggregator.refresh_until_held(Duration::from_secs(60), 5).await;
        let held = held_ids(&aggregator).len();
        let calls = *venue.list_calls.lock().unwrap();
        assert_eq!(held, 15);
        aggregator.refresh().await;
        assert_eq!(*venue.list_calls.lock().unwrap(), calls * 2, "refresh_until_held ran exactly one pass");
    }
}
