//! Canonical event discovery. Retained but no longer the primary path - see `api::v1::markets`,
//! which serves raw venue markets without the matching step. Event grouping only pays off once
//! several venues quote the same real-world event.

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::errors::ApiError;
use crate::models::event::{Event, EventSummary};
use crate::models::venue::Category;
use crate::repositories::health_repository::VenueHealth;
use crate::state::AppState;
use crate::utils::time::now_unix;

/// Lower/upper bound on `within_hours` - stops zero/negative windows. Upper bound is generous
/// (1 year) rather than tight: real venue data has turned out to span months (already-resolved
/// markets sitting alongside ones starting half a year out), wider than the original 7-day cap
/// anticipated - a caller deliberately asking for a huge window is a legitimate testing/ops need,
/// not something worth blocking.
const MIN_WITHIN_HOURS: i64 = 1;
const MAX_WITHIN_HOURS: i64 = 24 * 365;
const DEFAULT_WITHIN_HOURS: i64 = 72;

/// Query params for `GET /events`.
#[derive(Debug, Deserialize, IntoParams)]
pub struct ListEventsQuery {
    /// How far ahead to look for events starting soon. Clamped to [1, 8760] (1 year); default 72.
    pub within_hours: Option<i64>,
    /// Narrow to one category, e.g. "sports" or "crypto" - matched against the canonical event's
    /// own category (set when the event was first discovered), not any individual venue's own
    /// taxonomy.
    pub category: Option<String>,
}

#[utoipa::path(
    get,
    path = "/events",
    tag = "discovery",
    params(ListEventsQuery),
    responses((status = 200, body = Vec<EventSummary>, description = "Events starting within `within_hours` (default 72), optionally filtered by `category`, or already live and not yet resolved - soonest first."))
)]
/// Lists events happening soon, summarized, soonest-first.
pub async fn list_events(
    State(state): State<AppState>,
    Query(query): Query<ListEventsQuery>,
) -> Json<Vec<EventSummary>> {
    let within_hours = query
        .within_hours
        .unwrap_or(DEFAULT_WITHIN_HOURS)
        .clamp(MIN_WITHIN_HOURS, MAX_WITHIN_HOURS);
    let category = query.category.map(Category::from);

    let now = now_unix();
    let graph = state.aggregator.graph.read().await;
    let events = graph
        .events_within(now, within_hours, category.as_ref())
        .into_iter()
        .map(|event| EventSummary::from_event(event, now))
        .collect();
    Json(events)
}

#[utoipa::path(
    get,
    path = "/events/{id}/markets",
    tag = "discovery",
    params(("id" = String, Path, description = "Canonical event_id, e.g. 'evt_1'")),
    responses(
        (status = 200, body = Event, description = "The full event, with every venue's market attached to each outcome."),
        (status = 404, description = "No event with that id"),
    )
)]
/// Fetches one event's full detail, including every attached venue market.
pub async fn get_event_markets(
    State(state): State<AppState>,
    Path(event_id): Path<String>,
) -> Result<Json<Event>, ApiError> {
    let graph = state.aggregator.graph.read().await;
    graph
        .get(&event_id)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("no event '{event_id}'")))
}

#[utoipa::path(
    get,
    path = "/venues/health",
    tag = "discovery",
    responses((status = 200, body = Vec<VenueHealth>, description = "Live reachability/failure-streak state for every registered venue."))
)]
/// Reports live health for every registered venue.
pub async fn venue_health(State(state): State<AppState>) -> Json<Vec<VenueHealth>> {
    Json(state.aggregator.health.snapshot())
}
