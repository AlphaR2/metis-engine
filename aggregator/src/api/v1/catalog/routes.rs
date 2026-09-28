//! `/v1` - the events-first API for integrators.
//!
//! Everything here reads the canonical event graph (`engine`), rebuilt after every refresh:
//! events with rich metadata, their questions as full sentences, each outcome's listings across
//! venues rated best-first, and a flat markets view that always carries its event and question so
//! it is never a bare label. Lists use opaque cursors and return an `ETag`; ids are stable across
//! rebuilds and restarts.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::engine::graph::{EventGraph, EventNode, EventStatus, QuestionKind, QuestionNode};
use crate::engine::matching::{MatchDecision, MatchOverrides};
use crate::engine::text::fold;
use crate::errors::ApiError;
use crate::models::event::{VenueMarketRef, VenueSide};
use crate::models::routing::Recommendation;
use crate::models::venue::{Category, VenueCapabilities, VenueMarket};
use crate::repositories::health_repository::VenueHealth;
use crate::repositories::market_repository::CategoryBreakdown;
use crate::services::routing::{find_best_market, strategy_by_name};
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

/// The first outcomes of an event's main question, with their best price across venues.
#[derive(Debug, Serialize, ToSchema)]
pub struct Headline {
    pub question_id: String,
    pub text: String,
    pub kind: QuestionKind,
    pub outcomes: Vec<HeadlineOutcome>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HeadlineOutcome {
    pub id: String,
    pub label: String,
    #[schema(value_type = Option<String>)]
    pub best_price: Option<Decimal>,
    pub best_venue: Option<String>,
}

/// An event as a list item: its metadata and headline question, without every listing.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventCard {
    pub id: String,
    pub slug: String,
    /// A headline naming what the event is about - never a question; its questions carry those.
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// One line on what the event covers: category, questions and venues, closing dates.
    pub context: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subcategory: Option<String>,
    pub domain: String,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    pub close_time: i64,
    pub status: EventStatus,
    #[schema(value_type = String)]
    pub volume_usdc: Decimal,
    pub venues: Vec<String>,
    pub question_count: usize,
    pub headline: Option<Headline>,
}

fn headline(question: &QuestionNode) -> Headline {
    let mut outcomes: Vec<HeadlineOutcome> = question
        .outcomes
        .iter()
        .map(|o| HeadlineOutcome {
            id: o.id.clone(),
            label: o.label.clone(),
            best_price: o.best_price(),
            best_venue: o.listings.first().map(|l| l.venue_id.clone()),
        })
        .collect();
    // For a many-option question, the most likely options lead.
    if question.kind == QuestionKind::Multi {
        outcomes.sort_by_key(|o| std::cmp::Reverse(o.best_price));
        outcomes.truncate(4);
    }
    Headline { question_id: question.id.clone(), text: question.text.clone(), kind: question.kind, outcomes }
}

fn venues_of(event: &EventNode) -> Vec<String> {
    let mut venues: Vec<String> = event.questions.iter().flat_map(|q| q.venue_ids()).map(str::to_string).collect();
    venues.sort();
    venues.dedup();
    venues
}

fn card(event: &EventNode) -> EventCard {
    EventCard {
        id: event.id.clone(),
        slug: event.slug.clone(),
        title: event.title.clone(),
        subtitle: event.subtitle.clone(),
        context: event.context.clone(),
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
        venues: venues_of(event),
        question_count: event.question_count,
        headline: event.questions.first().map(headline),
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

/// Sort key for `sort=best`: events on more venues first (that is what the aggregator adds), then
/// by traded volume, then soonest to close.
fn best_order(a: &EventNode, b: &EventNode) -> std::cmp::Ordering {
    b.venue_count.cmp(&a.venue_count).then(b.volume_usdc.cmp(&a.volume_usdc)).then(a.close_time.cmp(&b.close_time))
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
        .filter(|e| query.venue.as_ref().is_none_or(|v| e.questions.iter().any(|q| q.venue_ids().contains(&v.as_str()))))
        .filter(|e| query.min_venues.is_none_or(|n| e.venue_count >= n))
        .filter(|e| {
            needle.as_ref().is_none_or(|n| {
                fold(&e.title).contains(n.as_str())
                    || e.questions.iter().any(|q| fold(&q.text).contains(n.as_str()))
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
    tag = "v1",
    params(ListEventsQuery),
    responses(
        (status = 200, body = EventsPage, description = "Event cards: metadata plus the headline question with best prices."),
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

#[utoipa::path(
    get,
    path = "/v1/events/{id}",
    tag = "v1",
    params(("id" = String, Path, description = "Event id (`evt_…`); an id retired by a merge still resolves.")),
    responses(
        (status = 200, body = EventNode, description = "The full event: metadata, questions, outcomes, and every venue listing rated best-first."),
        (status = 404, description = "No such event."),
    )
)]
/// One event in full.
pub async fn get_event(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Result<Response, ApiError> {
    let graph = state.aggregator.events.read().await.clone();
    let event = graph.event(&id).ok_or_else(|| ApiError::NotFound(format!("no event '{id}'")))?;
    Ok(cached_json(&headers, graph.built_at, &format!("event/{id}"), event))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct QuestionResponse {
    pub event: EventCard,
    pub question: QuestionNode,
}

#[utoipa::path(
    get,
    path = "/v1/events/{id}/questions/{question_id}",
    tag = "v1",
    params(
        ("id" = String, Path, description = "Event id"),
        ("question_id" = String, Path, description = "Question id (`q_…`)"),
    ),
    responses((status = 200, body = QuestionResponse), (status = 404, description = "No such event or question."))
)]
/// One question with every outcome's listings and their rating breakdowns.
pub async fn get_question(State(state): State<AppState>, Path((id, question_id)): Path<(String, String)>) -> Result<Json<QuestionResponse>, ApiError> {
    let graph = state.aggregator.events.read().await.clone();
    let event = graph.event(&id).ok_or_else(|| ApiError::NotFound(format!("no event '{id}'")))?;
    let question = event
        .questions
        .iter()
        .find(|q| q.id == question_id)
        .ok_or_else(|| ApiError::NotFound(format!("no question '{question_id}' in event '{id}'")))?;
    Ok(Json(QuestionResponse { event: card(event), question: question.clone() }))
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

/// One venue market with the event and question it belongs to.
#[derive(Debug, Serialize, ToSchema)]
pub struct MarketWithContext {
    #[serde(flatten)]
    pub market: VenueMarket,
    pub event_id: Option<String>,
    pub event_title: Option<String>,
    pub question_id: Option<String>,
    pub question_text: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MarketsPage {
    pub markets: Vec<MarketWithContext>,
    pub next_cursor: Option<String>,
    pub total: usize,
    pub built_at: i64,
}

fn with_context(market: VenueMarket, graph: &EventGraph) -> MarketWithContext {
    let context = graph.find_listing(&market.venue_id, &market.venue_market_id);
    MarketWithContext {
        event_id: context.map(|(e, _)| e.id.clone()),
        event_title: context.map(|(e, _)| e.title.clone()),
        question_id: context.map(|(_, q)| q.id.clone()),
        question_text: context.map(|(_, q)| q.text.clone()),
        market,
    }
}

#[utoipa::path(
    get,
    path = "/v1/markets",
    tag = "v1",
    params(ListMarketsV1Query),
    responses((status = 200, body = MarketsPage, description = "Every venue market, flat, each carrying its event and question."))
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

// ── Categories and venues ───────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryV1 {
    #[serde(flatten)]
    pub breakdown: CategoryBreakdown,
    /// Open events in this category.
    pub events: usize,
}

#[utoipa::path(get, path = "/v1/categories", tag = "v1", responses((status = 200, body = Vec<CategoryV1>)))]
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

// ── Routing ─────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct RouteRequest {
    pub question_id: String,
    pub outcome_id: String,
    /// Trade size in USDC, as a decimal string.
    pub size_usdc: String,
    /// `best_overall` (default), `best_price`, `best_liquidity`, `best_execution`.
    pub strategy: Option<String>,
}

#[utoipa::path(
    post,
    path = "/v1/route",
    tag = "v1",
    request_body = RouteRequest,
    responses(
        (status = 200, body = Recommendation, description = "Every venue listing for the outcome quoted live at this size and ranked."),
        (status = 400, description = "Bad size or strategy"),
        (status = 404, description = "No such question/outcome"),
        (status = 502, description = "Every venue failed to quote"),
    )
)]
/// The best venue to back one outcome at a size, from live quotes (fees included).
pub async fn route(State(state): State<AppState>, Json(req): Json<RouteRequest>) -> Result<Json<Recommendation>, ApiError> {
    match req.size_usdc.parse::<Decimal>() {
        Ok(size) if size > Decimal::ZERO => {}
        _ => return Err(ApiError::BadRequest(format!("size_usdc must be a positive number, got '{}'", req.size_usdc))),
    }
    let strategy = strategy_by_name(req.strategy.as_deref()).map_err(ApiError::BadRequest)?;
    let graph = state.aggregator.events.read().await.clone();
    let (_, question) = graph.question(&req.question_id).ok_or_else(|| ApiError::NotFound(format!("no question '{}'", req.question_id)))?;
    if question.rules_differ {
        tracing::debug!("routing across listings whose rules differ for {}", question.id);
    }
    let outcome = question
        .outcomes
        .iter()
        .find(|o| o.id == req.outcome_id)
        .ok_or_else(|| ApiError::NotFound(format!("no outcome '{}' in question '{}'", req.outcome_id, req.question_id)))?;
    let refs: Vec<VenueMarketRef> = outcome
        .listings
        .iter()
        .filter(|l| l.tradable)
        .map(|l| VenueMarketRef {
            venue_id: l.venue_id.clone(),
            venue_market_id: l.venue_market_id.clone(),
            venue_side: if l.side == "no" { VenueSide::No } else { VenueSide::Yes },
        })
        .collect();
    if refs.is_empty() {
        return Err(ApiError::NotFound(format!("no tradable listing for outcome '{}' - its markets are display-only", req.outcome_id)));
    }
    let recommendation = find_best_market(
        &refs,
        &req.size_usdc,
        strategy.as_ref(),
        |venue_id| state.aggregator.venue(venue_id),
        &state.aggregator.health,
        &state.aggregator.quotes,
    )
    .await?;
    Ok(Json(recommendation))
}

// ── Admin: matching ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
pub struct MatchesQuery {
    /// Lowest score to include; default 0.5.
    pub min_score: Option<f64>,
    /// Highest score to include; default 1.
    pub max_score: Option<f64>,
    /// Only merged (`true`) or only unmerged (`false`) decisions.
    pub merged: Option<bool>,
}

#[utoipa::path(
    get,
    path = "/admin/matches",
    tag = "admin",
    params(MatchesQuery),
    responses((status = 200, body = Vec<MatchDecision>), (status = 401, description = "Missing or invalid X-Admin-Token."))
)]
/// Cross-venue match decisions from the latest rebuild, with their feature breakdown - for
/// reviewing near-misses and tuning weights and dictionaries.
pub async fn list_matches(State(state): State<AppState>, Query(query): Query<MatchesQuery>) -> Json<Vec<MatchDecision>> {
    let decisions = state.aggregator.match_decisions.read().await.clone();
    let (min, max) = (query.min_score.unwrap_or(0.5), query.max_score.unwrap_or(1.0));
    let mut out: Vec<MatchDecision> = decisions
        .iter()
        .filter(|d| d.score >= min && d.score <= max)
        .filter(|d| query.merged.is_none_or(|m| d.merged == m))
        .cloned()
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    Json(out)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MatchOverrideRequest {
    /// `venue:market_id` of a market on one side, e.g. `panta:9xF…`.
    pub left: String,
    /// `venue:market_id` of a market on the other side.
    pub right: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OverridesResponse {
    #[schema(value_type = Object)]
    pub overrides: MatchOverrides,
}

async fn apply_override(state: &AppState, req: MatchOverrideRequest, merge: bool) -> Result<Json<OverridesResponse>, ApiError> {
    for side in [&req.left, &req.right] {
        if !side.contains(':') {
            return Err(ApiError::BadRequest(format!("'{side}' is not a venue:market_id pair")));
        }
    }
    if req.left == req.right {
        return Err(ApiError::BadRequest("left and right are the same listing".to_string()));
    }
    let overrides = state.aggregator.set_override(&req.left, &req.right, merge).await.map_err(ApiError::Upstream)?;
    Ok(Json(OverridesResponse { overrides }))
}

#[utoipa::path(
    post,
    path = "/admin/matches/merge",
    tag = "admin",
    request_body = MatchOverrideRequest,
    responses((status = 200, body = OverridesResponse), (status = 401, description = "Missing or invalid X-Admin-Token."))
)]
/// Forces two venues' markets into one question on every rebuild. Rebuilds immediately.
pub async fn merge_override(State(state): State<AppState>, Json(req): Json<MatchOverrideRequest>) -> Result<Json<OverridesResponse>, ApiError> {
    apply_override(&state, req, true).await
}

#[utoipa::path(
    post,
    path = "/admin/matches/split",
    tag = "admin",
    request_body = MatchOverrideRequest,
    responses((status = 200, body = OverridesResponse), (status = 401, description = "Missing or invalid X-Admin-Token."))
)]
/// Keeps two venues' markets apart on every rebuild, whatever the matcher scores. Rebuilds
/// immediately.
pub async fn split_override(State(state): State<AppState>, Json(req): Json<MatchOverrideRequest>) -> Result<Json<OverridesResponse>, ApiError> {
    apply_override(&state, req, false).await
}
