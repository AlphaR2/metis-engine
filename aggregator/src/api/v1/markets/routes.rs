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
use crate::repositories::market_repository::{CategoryBreakdown, MarketSort};
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
    /// The page size actually applied. With `quote_size` it is capped at the live-quote limit,
    /// so the next page starts at `offset + limit` using *this* value, not the one requested.
    pub limit: usize,
    /// Venues whose markets in this category are the last good copy because their most recent
    /// refresh failed. Empty when everything is current.
    pub stale_venues: Vec<String>,
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
        Some(raw) if !raw.is_empty() => Category::from(raw),
        _ => {
            let held = state.aggregator.markets.read().await.category_breakdown();
            return Err(ApiError::BadRequest(format!(
                "`category` is required - one of: {}",
                held.into_iter().map(|c| c.category).collect::<Vec<_>>().join(", ")
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
    let mut limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    // Every market on a live-quoted page is quoted, so the page itself is capped. Capping only the
    // quoting (and still advancing callers by the requested limit) silently skipped markets.
    if quote_size.is_some() {
        limit = limit.min(MAX_LIVE_QUOTES);
    }
    let offset = query.offset.unwrap_or(0);

    let now = now_unix();
    // Take the page and release the store's read lock before any network call. The live path
    // below can wait on venues for many seconds; holding the lock across that would queue the
    // refresh's write behind it, and every other reader behind the write.
    let (page, total, last_refreshed_at, stale_venues) = {
        let store = state.aggregator.markets.read().await;
        let mut matching = store.markets_in(&category, now, within_days * 86_400, query.venue.as_deref());
        let total = matching.len();
        sort_markets(&mut matching, sort, side);
        let page: Vec<VenueMarket> = matching.into_iter().skip(offset).take(limit).cloned().collect();
        (page, total, store.last_refreshed_at(), store.stale_venues_for(&category))
    };

    let (entries, scored_with) = match quote_size {
        // Cheap path: the catalog ordering is the answer.
        None => {
            let page: Vec<MarketEntry> = page
                .into_iter()
                .map(|market| MarketEntry { market, quoted_price: None, fully_executable: None })
                .collect();
            (page, ScoredWith::Catalog)
        }
        // Live path: re-price the page the cheap sort produced.
        Some(size) => {
            let shortlist: Vec<&VenueMarket> = page.iter().collect();
            let ranked = rank_with_quotes(
                &shortlist,
                size,
                side,
                sort,
                |venue_id| state.aggregator.venue(venue_id),
                &state.aggregator.health,
                &state.aggregator.quotes,
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
        limit,
        stale_venues,
    }))
}

/// Response for `GET /categories`.
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoriesResponse {
    /// Every category currently holding markets - the values `GET /markets?category=` accepts.
    pub categories: Vec<String>,
    /// Per category: the market count, and which venues hold it under which of their own names.
    pub breakdown: Vec<CategoryBreakdown>,
}

#[utoipa::path(
    get,
    path = "/categories",
    tag = "markets",
    responses((status = 200, body = CategoriesResponse, description = "Every category the venues currently hold markets in."))
)]
/// Lists the categories a caller may filter by. They come from the venues - each venue's own list
/// plus whatever its markets report - so a category a venue adds shows up here without a release.
/// Names that differ only in wording across venues are merged (see `CategoryAliases`).
pub async fn list_categories(State(state): State<AppState>) -> Json<CategoriesResponse> {
    let breakdown = state.aggregator.markets.read().await.category_breakdown();
    Json(CategoriesResponse { categories: breakdown.iter().map(|c| c.category.clone()).collect(), breakdown })
}
