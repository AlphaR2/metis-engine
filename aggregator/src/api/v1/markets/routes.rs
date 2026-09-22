//! `GET /markets` - the markets-first discovery endpoint.
//!
//! Serves raw venue markets, untransformed: what the venue reported is what a caller gets. The
//! canonical event graph (`/events`) still exists and groups markets across venues by title
//! similarity, but it only pays off when several venues quote the same real-world event, so it is
//! no longer the primary path.

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::errors::ApiError;
use crate::models::venue::{Category, VenueMarket};
use crate::repositories::market_repository::MarketSort;
use crate::services::markets::{MAX_LIVE_QUOTES, ScoredWith, rank_with_quotes};
use crate::services::maths::markets::sort_markets;
use crate::state::AppState;
use crate::utils::time::now_unix;

/// Bounds on `within_days`. The default is 7 - "closing within the week" - and the ceiling is
/// generous rather than tight, since a caller deliberately asking for a long horizon is a
/// legitimate ops/testing need.
const MIN_WITHIN_DAYS: i64 = 1;
const MAX_WITHIN_DAYS: i64 = 365;
const DEFAULT_WITHIN_DAYS: i64 = 7;

/// Bounds on `limit`, so one request can't ask for the entire store in a single page.
const MAX_LIMIT: usize = 200;
const DEFAULT_LIMIT: usize = 50;

/// Query params for `GET /markets`.
#[derive(Debug, Deserialize, IntoParams)]
pub struct ListMarketsQuery {
    /// **Required.** Which category to list, e.g. `sports`, `crypto`, `politics`. Venues use
    /// different category vocabularies; this is the canonical name, translated per venue.
    pub category: Option<String>,
    /// How far ahead to look, by **betting close time** - a market whose bets shut inside this
    /// many days. Not resolution time: those are different moments, often days apart. Clamped to
    /// [1, 365]; default 7.
    pub within_days: Option<i64>,
    /// `closing_soonest` (default) | `best_price` | `best_liquidity` | `best_execution` |
    /// `best_overall`.
    pub sort: Option<String>,
    /// Which side prices refer to - `yes` (default) or `no`.
    pub side: Option<String>,
    /// Narrow to a single venue, e.g. `jupiter`.
    pub venue: Option<String>,
    /// Trade size in USDC. When present, the top candidates are quoted live against their venues
    /// and re-ranked on real prices instead of catalog estimates.
    pub quote_size: Option<String>,
    /// Page size, clamped to [1, 200]; default 50.
    pub limit: Option<usize>,
    /// How many markets to skip, for paging.
    pub offset: Option<usize>,
}

/// One market in the response - the venue's own data, plus the live quote when one was taken.
#[derive(Debug, Serialize, ToSchema)]
pub struct MarketEntry {
    #[serde(flatten)]
    pub market: VenueMarket,
    /// Present only when `quote_size` was supplied: the real average fill price for that size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quoted_price: Option<String>,
    /// Present only when `quote_size` was supplied: whether the whole size fills.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fully_executable: Option<bool>,
}

/// `GET /markets` response.
#[derive(Debug, Serialize, ToSchema)]
pub struct ListMarketsResponse {
    pub markets: Vec<MarketEntry>,
    /// How many markets matched before `limit`/`offset` were applied.
    pub total: usize,
    /// Unix seconds of the last refresh, or `null` if none has completed. A UI should show this -
    /// an empty list from a stale store means something different from an empty list from a fresh
    /// one.
    pub last_refreshed_at: Option<i64>,
    /// `catalog` or `live_quotes`.
    pub scored_with: &'static str,
}

#[utoipa::path(
    get,
    path = "/markets",
    tag = "markets",
    params(ListMarketsQuery),
    responses(
        (status = 200, body = ListMarketsResponse, description = "Markets in the given category whose betting closes within `within_days`, ranked by `sort`."),
        (status = 400, description = "`category` missing, or an unknown `sort`/`quote_size`"),
    )
)]
/// Lists raw venue markets for one category, ranked.
pub async fn list_markets(
    State(state): State<AppState>,
    Query(query): Query<ListMarketsQuery>,
) -> Result<Json<ListMarketsResponse>, ApiError> {
    // Required, and enforced here rather than by making the field non-Option: axum's own
    // rejection for a missing query field is not a message to put in front of a UI.
    let category = match query.category.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Category::from(raw.to_string()),
        _ => {
            return Err(ApiError::BadRequest(format!(
                "`category` is required - one of: {}",
                Category::all().into_iter().map(String::from).collect::<Vec<_>>().join(", ")
            )));
        }
    };

    let sort = MarketSort::parse(query.sort.as_deref()).map_err(ApiError::BadRequest)?;
    let side = match query.side.as_deref() {
        None | Some("yes") => "yes",
        Some("no") => "no",
        Some(other) => return Err(ApiError::BadRequest(format!("unknown side '{other}' - expected 'yes' or 'no'"))),
    };
    let quote_size = match query.quote_size.as_deref() {
        None => None,
        Some(raw) => match raw.parse::<rust_decimal::Decimal>() {
            Ok(size) if size > rust_decimal::Decimal::ZERO => Some(size),
            _ => {
                return Err(ApiError::BadRequest(format!(
                    "quote_size must be a positive number, got '{raw}'"
                )));
            }
        },
    };

    let within_days = query.within_days.unwrap_or(DEFAULT_WITHIN_DAYS).clamp(MIN_WITHIN_DAYS, MAX_WITHIN_DAYS);
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = query.offset.unwrap_or(0);

    let now = now_unix();
    let store = state.aggregator.markets.read().await;
    let last_refreshed_at = store.last_refreshed_at();

    let mut matching = store.markets_in(&category, now, within_days * 86_400, query.venue.as_deref());
    let total = matching.len();
    sort_markets(&mut matching, sort, side);

    let (entries, scored_with) = match quote_size {
        // Cheap path: the catalog ordering is the answer.
        None => {
            let page: Vec<MarketEntry> = matching
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|market| MarketEntry { market: market.clone(), quoted_price: None, fully_executable: None })
                .collect();
            (page, ScoredWith::Catalog)
        }
        // Live path: re-price the shortlist the cheap sort produced. Only the markets that could
        // actually be reached on this page are worth quoting.
        Some(size) => {
            let shortlist: Vec<&VenueMarket> =
                matching.into_iter().skip(offset).take(limit.min(MAX_LIVE_QUOTES)).collect();
            let ranked = rank_with_quotes(
                &shortlist,
                size,
                side,
                sort,
                |venue_id| state.aggregator.venues.iter().find(|v| v.id() == venue_id).cloned(),
                &state.aggregator.health,
            )
            .await;

            let mut entries: Vec<MarketEntry> = ranked
                .into_iter()
                .map(|(market, scored)| MarketEntry {
                    market: market.clone(),
                    quoted_price: Some(scored.candidate.price.to_string()),
                    fully_executable: Some(scored.candidate.fully_executable),
                })
                .collect();
            // A caller who asked for closing-soonest still wants chronological order; quoting was
            // only about attaching real prices.
            if sort == MarketSort::ClosingSoonest {
                entries.sort_by_key(|e| e.market.close_time);
            }
            (entries, ScoredWith::LiveQuotes)
        }
    };

    Ok(Json(ListMarketsResponse {
        markets: entries,
        total,
        last_refreshed_at,
        scored_with: scored_with.as_str(),
    }))
}

/// Response for `GET /categories`.
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoriesResponse {
    /// Every category `GET /markets?category=` accepts.
    pub categories: Vec<String>,
}

#[utoipa::path(
    get,
    path = "/categories",
    tag = "markets",
    responses((status = 200, body = CategoriesResponse, description = "Every category the markets endpoint accepts."))
)]
/// Lists the categories a caller may filter by - `category` is required on `/markets`, so a UI
/// needs a way to discover the valid values rather than hardcoding them.
pub async fn list_categories() -> Json<CategoriesResponse> {
    Json(CategoriesResponse {
        categories: Category::all().into_iter().map(String::from).collect(),
    })
}
