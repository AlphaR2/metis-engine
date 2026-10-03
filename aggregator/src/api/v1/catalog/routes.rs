//! `/v1` - the events-first API for integrators.
//!
//! Everything here reads the event graph (`engine`), rebuilt after every refresh: **events**, our own
//! aggregation of markets about one real-world subject, and the **markets** under each event, exactly
//! as the venues sent them. Filtering, sorting and rating happen at event level; inside an event the
//! markets can be re-sorted. Lists use opaque cursors and return an `ETag`.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::engine::graph::{EventGraph, EventNode, EventStatus, MarketNode};
use crate::engine::text::fold;
use crate::errors::ApiError;
use crate::models::venue::{Category, VenueCapabilities, VenueMarket};
use crate::repositories::health_repository::VenueHealth;
use crate::repositories::market_repository::CategoryBreakdown;
use crate::state::AppState;
use crate::utils::time::now_unix;

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;

// ── Pagination and caching ──────────────────────────────────────────────────────────────────

/// Cursors are opaque to callers; today they carry an offset into the ordered result.
fn decode_cursor(cursor: Option<&str>) -> Result<usize, ApiError> {
    match cursor {
        None | Some("") => Ok(0),
        Some(raw) => raw
            .strip_prefix('o')
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| ApiError::BadRequest(format!("invalid cursor '{raw}'"))),
    }
}

fn page<T>(items: Vec<T>, cursor: Option<&str>, limit: Option<usize>) -> Result<(Vec<T>, Option<String>, usize), ApiError> {
    let offset = decode_cursor(cursor)?;
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let total = items.len();
    let next = (offset + limit < total).then(|| format!("o{}", offset + limit));
    Ok((items.into_iter().skip(offset).take(limit).collect(), next, total))
}

/// JSON with an `ETag` derived from the graph's build time and the request, plus a short public
/// cache lifetime. A matching `If-None-Match` gets a bodyless 304.
fn cached_json<T: Serialize>(headers: &HeaderMap, built_at: i64, request_key: &str, body: &T) -> Response {
    let etag = format!("W/\"{built_at:x}-{}\"", &crate::engine::graph::stable_id("r", request_key)[2..]);
    let not_modified = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).is_some_and(|v| v == etag);
    let mut response = if not_modified { StatusCode::NOT_MODIFIED.into_response() } else { Json(body).into_response() };
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&etag) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=30"));
    response
}

// ── Events ──────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListEventsQuery {
    /// Shared category name (see `/v1/categories`).
    pub category: Option<String>,
    /// What the event is about, by extraction: `sports`, `crypto`, `finance`, `commodity`,
    /// `politics`, `weather`, `other`.
    pub domain: Option<String>,
    /// `open` (default: upcoming and live), `upcoming`, `live`, `closed`, `all`.
    pub status: Option<String>,
    /// Only events whose earliest betting close is within this many days.
    pub within_days: Option<i64>,
    /// Text search over title, tags and entity names.
    pub q: Option<String>,
    /// Only events with a listing on this venue.
    pub venue: Option<String>,
    /// Only events listed on at least this many venues (2 = cross-venue only).
    pub min_venues: Option<usize>,
    /// `best` (default), `closing_soonest`, `volume`.
    pub sort: Option<String>,
    pub cursor: Option<String>,
    /// Page size, 1-100; default 20.
    pub limit: Option<usize>,
}

/// A market in an event card's preview: the venue's own title and prices.
#[derive(Debug, Serialize, ToSchema)]
pub struct MarketPreview {
    pub venue_id: String,
    pub venue_market_id: String,
    pub title: String,
    #[schema(value_type = Option<String>)]
    pub yes_price: Option<String>,
    #[schema(value_type = Option<String>)]
    pub no_price: Option<String>,
    #[schema(value_type = Option<String>)]
    pub volume_usdc: Option<Decimal>,
}

/// An event as a list item: our metadata and a preview of its best markets.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventCard {
    pub id: String,
    pub slug: String,
    /// A headline naming the subject, never a question.
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subcategory: Option<String>,
    pub domain: String,
    pub tags: Vec<String>,
    /// Absent when no venue sent one; clients draw a category fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    pub close_time: i64,
    pub status: EventStatus,
    #[schema(value_type = String)]
    pub volume_usdc: Decimal,
    pub venues: Vec<String>,
    pub market_count: usize,
    /// Our 0-100 rating of the event.
    pub rating: u8,
    /// Why its markets were grouped: `venue_event`, `match`, `price`, `terms`, `ai` or `single`.
    pub grouping: String,
    /// The two or three best-rated markets, as the venues listed them.
    pub top_markets: Vec<MarketPreview>,
}

fn preview(node: &MarketNode) -> MarketPreview {
    MarketPreview {
        venue_id: node.market.venue_id.clone(),
        venue_market_id: node.market.venue_market_id.clone(),
        title: node.market.title.clone(),
        yes_price: node.market.yes_price.clone(),
        no_price: node.market.no_price.clone(),
        volume_usdc: node.market.liquidity,
    }
}

fn card(event: &EventNode) -> EventCard {
    EventCard {
        id: event.id.clone(),
        slug: event.slug.clone(),
        title: event.title.clone(),
        subtitle: event.subtitle.clone(),
        description: event.description.clone(),
        category: event.category.clone(),
        subcategory: event.subcategory.clone(),
        domain: event.domain.clone(),
        tags: event.tags.clone(),
        image_url: event.image_url.clone(),
        start_time: event.start_time,
        close_time: event.close_time,
        status: event.status,
        volume_usdc: event.volume_usdc,
        venues: event.venues.clone(),
        market_count: event.market_count,
        rating: event.rating.score,
        grouping: event.grouping.reason.clone(),
        top_markets: event.markets.iter().take(3).map(preview).collect(),
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EventsPage {
    pub events: Vec<EventCard>,
    /// Pass back as `cursor` for the next page; absent on the last page.
    pub next_cursor: Option<String>,
    /// Events matching the filters, across all pages.
    pub total: usize,
    /// Unix seconds when the graph was built.
    pub built_at: i64,
}

fn status_filter(raw: Option<&str>) -> Result<Vec<EventStatus>, ApiError> {
    Ok(match raw.unwrap_or("open") {
        "open" => vec![EventStatus::Upcoming, EventStatus::Live],
        "upcoming" => vec![EventStatus::Upcoming],
        "live" => vec![EventStatus::Live],
        "closed" => vec![EventStatus::Closed],
        "all" => vec![EventStatus::Upcoming, EventStatus::Live, EventStatus::Closed],
        other => return Err(ApiError::BadRequest(format!("unknown status '{other}' - expected open, upcoming, live, closed or all"))),
    })
}

/// Sort key for `sort=best`: our event rating (activity, venue coverage, depth), then volume, then
/// soonest to close.
fn best_order(a: &EventNode, b: &EventNode) -> std::cmp::Ordering {
    b.rating.score.cmp(&a.rating.score).then(b.volume_usdc.cmp(&a.volume_usdc)).then(a.close_time.cmp(&b.close_time))
}

fn filter_events<'g>(graph: &'g EventGraph, query: &ListEventsQuery, now: i64) -> Result<Vec<&'g EventNode>, ApiError> {
    let statuses = status_filter(query.status.as_deref())?;
    let category = query.category.as_deref().map(Category::from);
    let domain = query.domain.as_deref().map(|d| d.trim().to_lowercase());
    let horizon = query.within_days.map(|d| now + d.clamp(1, 3650) * 86_400);
    let needle = query.q.as_deref().map(fold).filter(|q| !q.trim().is_empty());
    let mut events: Vec<&EventNode> = graph
        .events
        .iter()
        .filter(|e| statuses.contains(&e.status))
        .filter(|e| category.as_ref().is_none_or(|c| e.category == c.as_str()))
        .filter(|e| domain.as_ref().is_none_or(|d| &e.domain == d))
        .filter(|e| horizon.is_none_or(|h| e.close_time > 0 && e.close_time <= h))
        .filter(|e| query.venue.as_ref().is_none_or(|v| e.venues.contains(v)))
        .filter(|e| query.min_venues.is_none_or(|n| e.venue_count >= n))
        .filter(|e| {
            needle.as_ref().is_none_or(|n| {
                fold(&e.title).contains(n.as_str())
                    || e.markets.iter().any(|m| fold(&m.market.title).contains(n.as_str()))
                    || e.tags.iter().any(|t| fold(t).contains(n.as_str()))
                    || e.entities.iter().any(|x| fold(&x.name).contains(n.as_str()))
            })
        })
        .collect();
    match query.sort.as_deref().unwrap_or("best") {
        "best" => events.sort_by(|a, b| best_order(a, b)),
        "closing_soonest" => events.sort_by_key(|e| (e.close_time == 0, e.close_time)),
        "volume" => events.sort_by_key(|e| std::cmp::Reverse(e.volume_usdc)),
        other => return Err(ApiError::BadRequest(format!("unknown sort '{other}' - expected best, closing_soonest or volume"))),
    }
    Ok(events)
}

#[utoipa::path(
    get,
    path = "/v1/events",
    operation_id = "v1_list_events",
    tag = "v1",
    params(ListEventsQuery),
    responses(
        (status = 200, body = EventsPage, description = "Event cards: our aggregation's metadata plus a preview of its best markets."),
        (status = 304, description = "Unchanged since the given ETag."),
        (status = 400, description = "Unknown status/sort or invalid cursor."),
    )
)]
/// Lists events, newest graph, filtered and sorted.
pub async fn list_events(State(state): State<AppState>, headers: HeaderMap, Query(query): Query<ListEventsQuery>) -> Result<Response, ApiError> {
    let graph = state.aggregator.events.read().await.clone();
    let events = filter_events(&graph, &query, now_unix())?;
    let (events, next_cursor, total) = page(events, query.cursor.as_deref(), query.limit)?;
    let body = EventsPage { events: events.into_iter().map(card).collect(), next_cursor, total, built_at: graph.built_at };
    Ok(cached_json(&headers, graph.built_at, &format!("events?{query:?}"), &body))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct EventQuery {
    /// How to order the event's markets: `rating` (default, ours), `volume`, `closing`
    /// (soonest first) or `price` (highest YES price first).
    pub sort: Option<String>,
    /// Only the markets listed on this venue.
    pub venue: Option<String>,
}

fn yes_price(node: &MarketNode) -> Decimal {
    node.market.yes_price.as_deref().and_then(|p| p.parse().ok()).unwrap_or(Decimal::ZERO)
}

#[utoipa::path(
    get,
    path = "/v1/events/{id}",
    tag = "v1",
    params(("id" = String, Path, description = "Event id (`evt_…`)."), EventQuery),
    responses(
        (status = 200, body = EventNode, description = "The event with its markets exactly as the venues sent them, each with our rating."),
        (status = 400, description = "Unknown sort."),
        (status = 404, description = "No such event."),
    )
)]
/// One event in full, with its markets sorted and filtered as asked.
pub async fn get_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<EventQuery>,
) -> Result<Response, ApiError> {
    let graph = state.aggregator.events.read().await.clone();
    let event = graph.event(&id).ok_or_else(|| ApiError::NotFound(format!("no event '{id}'")))?;
    let mut event = event.clone();
    if let Some(venue) = &query.venue {
        event.markets.retain(|m| &m.market.venue_id == venue);
    }
    match query.sort.as_deref().unwrap_or("rating") {
        "rating" => {}
        "volume" => event.markets.sort_by_key(|m| std::cmp::Reverse(m.volume())),
        "closing" => event.markets.sort_by_key(|m| (m.market.close_time == 0, m.market.close_time)),
        "price" => event.markets.sort_by_key(|m| std::cmp::Reverse(yes_price(m))),
        other => return Err(ApiError::BadRequest(format!("unknown sort '{other}' - expected rating, volume, closing or price"))),
    }
    Ok(cached_json(&headers, graph.built_at, &format!("event/{id}?{query:?}"), &event))
}

// ── Markets ─────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListMarketsV1Query {
    /// Shared category name. Optional here, unlike the legacy `/markets`.
    pub category: Option<String>,
    pub venue: Option<String>,
    /// Only markets whose betting closes within this many days.
    pub within_days: Option<i64>,
    /// `closing_soonest` (default) or `volume`.
    pub sort: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

/// One venue market with the event it belongs to.
#[derive(Debug, Serialize, ToSchema)]
pub struct MarketWithContext {
    #[serde(flatten)]
    pub market: VenueMarket,
    pub event_id: Option<String>,
    pub event_title: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MarketsPage {
    pub markets: Vec<MarketWithContext>,
    pub next_cursor: Option<String>,
    pub total: usize,
    pub built_at: i64,
}

fn with_context(market: VenueMarket, graph: &EventGraph) -> MarketWithContext {
    let context = graph.find_market(&market.venue_id, &market.venue_market_id);
    MarketWithContext { event_id: context.map(|(e, _)| e.id.clone()), event_title: context.map(|(e, _)| e.title.clone()), market }
}

#[utoipa::path(
    get,
    path = "/v1/markets",
    operation_id = "v1_list_markets",
    tag = "v1",
    params(ListMarketsV1Query),
    responses((status = 200, body = MarketsPage, description = "Every venue market, flat, each carrying its event."))
)]
/// The markets-only path: every listing flat, with event context so it is never a bare label.
pub async fn list_markets(State(state): State<AppState>, headers: HeaderMap, Query(query): Query<ListMarketsV1Query>) -> Result<Response, ApiError> {
    let now = now_unix();
    let graph = state.aggregator.events.read().await.clone();
    let category = query.category.as_deref().map(Category::from);
    let horizon = query.within_days.map(|d| now + d.clamp(1, 3650) * 86_400);
    let mut markets: Vec<VenueMarket> = state
        .aggregator
        .markets
        .read()
        .await
        .all_markets()
        .into_iter()
        .filter(|m| m.is_bettable(now))
        .filter(|m| category.as_ref().is_none_or(|c| &m.category == c))
        .filter(|m| query.venue.as_ref().is_none_or(|v| &m.venue_id == v))
        .filter(|m| horizon.is_none_or(|h| m.close_time > 0 && m.close_time <= h))
        .collect();
    match query.sort.as_deref().unwrap_or("closing_soonest") {
        "closing_soonest" => markets.sort_by_key(|m| (m.close_time == 0, m.close_time, m.venue_market_id.clone())),
        "volume" => markets.sort_by(|a, b| b.liquidity.cmp(&a.liquidity).then(a.venue_market_id.cmp(&b.venue_market_id))),
        other => return Err(ApiError::BadRequest(format!("unknown sort '{other}' - expected closing_soonest or volume"))),
    }
    let (markets, next_cursor, total) = page(markets, query.cursor.as_deref(), query.limit)?;
    let body = MarketsPage { markets: markets.into_iter().map(|m| with_context(m, &graph)).collect(), next_cursor, total, built_at: graph.built_at };
    Ok(cached_json(&headers, graph.built_at, &format!("markets?{query:?}"), &body))
}

#[utoipa::path(
    get,
    path = "/v1/markets/{venue_id}/{venue_market_id}",
    tag = "v1",
    params(("venue_id" = String, Path), ("venue_market_id" = String, Path)),
    responses((status = 200, body = MarketWithContext), (status = 404, description = "Not held."))
)]
/// One market with its event context.
pub async fn get_market(State(state): State<AppState>, Path((venue_id, venue_market_id)): Path<(String, String)>) -> Result<Json<MarketWithContext>, ApiError> {
    let graph = state.aggregator.events.read().await.clone();
    let market = state
        .aggregator
        .markets
        .read()
        .await
        .all_markets()
        .into_iter()
        .find(|m| m.venue_id == venue_id && m.venue_market_id == venue_market_id)
        .ok_or_else(|| ApiError::NotFound(format!("no market '{venue_market_id}' on '{venue_id}'")))?;
    Ok(Json(with_context(market, &graph)))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LiveMarket {
    #[serde(flatten)]
    pub market: MarketWithContext,
    /// When the venue was asked, unix seconds.
    pub fetched_at: i64,
    /// True when the catalog also holds this market, so its event context and any field the
    /// venue's single-market record lacks (Jupiter's category) come from that copy.
    pub held: bool,
}

#[utoipa::path(
    get,
    path = "/v1/markets/{venue_id}/{venue_market_id}/live",
    tag = "v1",
    params(("venue_id" = String, Path), ("venue_market_id" = String, Path)),
    responses(
        (status = 200, body = LiveMarket),
        (status = 404, description = "Unknown venue, or the venue has no such market."),
        (status = 502, description = "The venue could not be reached.")
    )
)]
/// One market read from the venue right now, not from the refresh copy. Costs a venue call, so it
/// is rate-limited like a live quote. Prefer the plain route for anything a refresh-old copy serves.
pub async fn get_market_live(State(state): State<AppState>, Path((venue_id, venue_market_id)): Path<(String, String)>) -> Result<Json<LiveMarket>, ApiError> {
    let venue = state.aggregator.venue(&venue_id).ok_or_else(|| ApiError::NotFound(format!("no venue '{venue_id}'")))?;
    let mut market = venue.get_market(&venue_market_id).await?;
    if market.venue_id.is_empty() {
        market.venue_id = venue.id().to_string();
    }
    let stored = state
        .aggregator
        .markets
        .read()
        .await
        .all_markets()
        .into_iter()
        .find(|m| m.venue_id == market.venue_id && m.venue_market_id == market.venue_market_id);
    let held = stored.is_some();
    if let Some(stored) = stored {
        if market.venue_category.is_empty() {
            market.venue_category = stored.venue_category;
            market.category = stored.category;
        }
        if market.event.is_none() {
            market.event = stored.event;
        }
    }
    let graph = state.aggregator.events.read().await.clone();
    Ok(Json(LiveMarket { market: with_context(market, &graph), fetched_at: now_unix(), held }))
}

// ── Categories and venues ───────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryV1 {
    #[serde(flatten)]
    pub breakdown: CategoryBreakdown,
    /// Open events in this category.
    pub events: usize,
}

#[utoipa::path(get, path = "/v1/categories", operation_id = "v1_list_categories", tag = "v1", responses((status = 200, body = Vec<CategoryV1>)))]
/// Categories as the venues report them, with each venue's own names and event/market counts.
pub async fn list_categories(State(state): State<AppState>) -> Json<Vec<CategoryV1>> {
    let graph = state.aggregator.events.read().await.clone();
    let breakdown = state.aggregator.markets.read().await.category_breakdown();
    Json(
        breakdown
            .into_iter()
            .map(|b| CategoryV1 {
                events: graph.events.iter().filter(|e| e.category == b.category && e.status != EventStatus::Closed).count(),
                breakdown: b,
            })
            .collect(),
    )
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VenueV1 {
    pub id: String,
    #[schema(value_type = Object)]
    pub capabilities: VenueCapabilities,
    pub health: Option<VenueHealth>,
    pub markets: usize,
    /// True when the venue is serving its last good catalog because its latest refresh failed.
    pub stale: bool,
}

#[utoipa::path(get, path = "/v1/venues", tag = "v1", responses((status = 200, body = Vec<VenueV1>)))]
/// Enabled venues, what they can do, and how healthy they are right now.
pub async fn list_venues(State(state): State<AppState>) -> Json<Vec<VenueV1>> {
    let health = state.aggregator.health.snapshot();
    let buckets = state.aggregator.markets.read().await.bucket_statuses();
    Json(
        state
            .aggregator
            .venues
            .iter()
            .map(|v| VenueV1 {
                id: v.id().to_string(),
                capabilities: v.capabilities(),
                health: health.iter().find(|h| h.venue_id == v.id()).cloned(),
                markets: buckets.iter().filter(|b| b.venue_id == v.id()).map(|b| b.markets).sum(),
                stale: buckets.iter().any(|b| b.venue_id == v.id() && b.stale),
            })
            .collect(),
    )
}
