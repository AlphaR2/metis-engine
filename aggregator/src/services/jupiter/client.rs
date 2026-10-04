//! Raw HTTP/wire layer for Jupiter Prediction's REST API. Pure request/response plumbing - no
//! business logic (which side of the book to walk is a trading decision, made in `service.rs`).
//! Wire types live in `crate::models::venue::jupiter`.

use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::venue::jupiter::{
    JupEvent, JupEventsResponse, JupMarket, JupOrderbookResponse, micro_usd_to_decimal_string,
};
use crate::models::venue::{Category, ListParams, MarketPage, VenueEntity, VenueEvent, VenueMarket};
use crate::utils::http::{parse_json_with_diagnostics, venue_url};

/// How many events to request per page - Jupiter's pagination is index-range based
/// (`start`/`end`), not a page-size parameter, so this is just the width of range we ask for.
const PAGE_SIZE: i64 = 50;

/// Flattens one market, together with its parent event, into a venue-agnostic `VenueMarket`.
///
/// The event is carried rather than discarded because Jupiter puts the *question* there and only
/// the *outcome* on the market. Keeping just the market is what turned this catalog into a list of
/// bare labels - "Up", "Down", "160-179", "September 21" - with nothing saying what was being
/// predicted.
pub(crate) fn market_to_venue_market(event: Option<&JupEvent>, market: &JupMarket) -> VenueMarket {
    let pricing = market.pricing.as_ref();
    // Jupiter has no separate resolved flag - "open" is the only tradeable status observed
    // ("closed" markets confirmed in real data still carry a stale price snapshot, not a live
    // one), so anything else is treated as dead.
    let dead = !market.status.eq_ignore_ascii_case("open");

    let event_category = event.map(|e| e.category.as_str()).unwrap_or("");
    let non_blank = |s: &Option<String>| s.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    // Only surface an event that actually names a question; an empty title would just be a second
    // blank line in the UI.
    let venue_event = event.and_then(|e| {
        let metadata = e.metadata.as_ref()?;
        let title = metadata.title.trim();
        if title.is_empty() {
            return None;
        }
        Some(VenueEvent {
            event_id: e.event_id.clone(),
            title: title.to_string(),
            subtitle: non_blank(&metadata.subtitle),
            image_url: non_blank(&metadata.image_url),
            slug: non_blank(&metadata.slug),
            description: None,
            tags: e.tags.clone(),
            subcategory: non_blank(&e.subcategory),
            league: non_blank(&metadata.league),
            start_time: e.begin_at,
            volume_usdc: e.volume_usd.as_deref().and_then(micro_usd_str_to_decimal),
            volume_24h_usdc: e.volume_24hr.as_deref().and_then(micro_usd_str_to_decimal),
            close_condition: non_blank(&e.close_condition),
            extra: e.extra.clone(),
        })
    });

    VenueMarket {
        venue_id: "jupiter".to_string(),
        venue_market_id: market.market_id.clone(),
        title: market.title.clone(),
        event: venue_event,
        rules: market.rules_primary.as_ref().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()),
        category: Category::from(event_category),
        venue_category: event_category.to_string(),
        phase: market.status.clone(),
        start_time: market.open_time,
        close_time: market.close_time,
        // `resolveAt` is null while a market is open, so fall back to the betting cutoff rather
        // than reporting a resolution time of 0.
        resolution_time: market.resolve_at.unwrap_or(market.close_time),
        yes_price: pricing.and_then(|p| p.buy_yes_price_usd).map(micro_usd_to_decimal_string),
        no_price: pricing.and_then(|p| p.buy_no_price_usd).map(micro_usd_to_decimal_string),
        liquidity: pricing.and_then(|p| p.volume).and_then(Decimal::from_f64_retain),
        dead,
        outcome_labels: market.outcomes.clone(),
        entity: market.team.as_ref().map(|team| VenueEntity {
            name: team.name.clone(),
            abbreviation: team.abbreviation.clone(),
            image_url: team.image_url.clone(),
        }),
        market_kind: non_blank(&market.sports_market_type),
        provider: non_blank(&market.provider),
        extra: market.extra.clone(),
        ..Default::default()
    }
}

/// Jupiter's event volumes are micro-USD integers sent as strings.
fn micro_usd_str_to_decimal(raw: &str) -> Option<Decimal> {
    raw.trim().parse::<i64>().ok().map(|micro| Decimal::new(micro, 6).normalize())
}

/// Calls Jupiter's `GET /events` and flattens every nested market into one page. `params.cursor`
/// (if set) is the `start` offset from a previous page's `next_cursor`, encoded as a string -
/// Jupiter itself has no cursor token, just index ranges.
pub async fn list_markets(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    params: ListParams,
) -> Result<MarketPage, AdapterError> {
    let start: i64 = params.cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
    let end = start + PAGE_SIZE;

    let mut query: Vec<(&str, String)> = vec![
        ("includeMarkets", "true".to_string()),
        ("start", start.to_string()),
        ("end", end.to_string()),
        // Most-traded first. Jupiter's `sortBy` accepts only `volume|volume24hr|beginAt` - there
        // is no close-time ordering to ask for - and the refresh caps how deep it pages, so what
        // that cap keeps matters. Taking the busiest markets first means the cap trims the tail
        // nobody was going to trade rather than an arbitrary slice.
        ("sortBy", "volume".to_string()),
        ("sortDirection", "desc".to_string()),
    ];
    if let Some(sort_by) = params.sort_by {
        // An explicit caller-supplied sort wins over the default above.
        query.retain(|(k, _)| *k != "sortBy");
        query.push(("sortBy", sort_by));
    }
    if let Some(sort_direction) = params.sort_direction {
        query.retain(|(k, _)| *k != "sortDirection");
        query.push(("sortDirection", sort_direction));
    }
    if let Some(filter) = params.filter {
        query.push(("filter", filter));
    }
    if let Some(category) = params.category {
        // Always Jupiter's own spelling - the refresh passes back what `filter_categories` gave.
        query.push(("category", category));
    }

    let response = client
        .get(venue_url(base_url, &["events"])?)
        .headers(crate::utils::http::api_key_headers("x-api-key", api_key))
        .query(&query)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AdapterError::Venue(e.to_string()))?;

    let response: JupEventsResponse = parse_json_with_diagnostics(response, "jupiter list_markets")
        .await
        .map_err(AdapterError::Parse)?;

    let items = response
        .data
        .iter()
        .flat_map(|event: &JupEvent| event.markets.iter().map(|m| market_to_venue_market(Some(event), m)))
        .collect();

    let next_cursor = response.pagination.has_next.then(|| response.pagination.end.to_string());

    Ok(MarketPage { items, next_cursor })
}

/// Calls Jupiter's `GET /markets/{id}` for one market.
pub async fn get_market(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    venue_market_id: &str,
) -> Result<VenueMarket, AdapterError> {
    let response = client
        .get(venue_url(base_url, &["markets", venue_market_id])?)
        .headers(crate::utils::http::api_key_headers("x-api-key", api_key))
        .send()
        .await?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(AdapterError::MarketNotFound(venue_market_id.to_string()));
    }

    // The bare market endpoint has no parent-event category at all - Jupiter's own Market schema
    // doesn't carry one, category is purely an Event-level property. Left empty
    // same discipline as before; list_markets (which does have the event) is the real source.
    let response = response.error_for_status().map_err(|e| AdapterError::Venue(e.to_string()))?;
    let market: JupMarket = parse_json_with_diagnostics(response, "jupiter get_market")
        .await
        .map_err(AdapterError::Parse)?;

    Ok(market_to_venue_market(None, &market))
}

/// Calls Jupiter's `GET /orderbook/{id}` and returns the raw book, both sides. Which side to
/// walk and how is a trading decision - that's `service.rs`'s job, not this one's.
#[allow(dead_code)]
pub async fn get_orderbook(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    venue_market_id: &str,
) -> Result<JupOrderbookResponse, AdapterError> {
    let response = client
        .get(venue_url(base_url, &["orderbook", venue_market_id])?)
        .headers(crate::utils::http::api_key_headers("x-api-key", api_key))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AdapterError::Venue(e.to_string()))?;

    parse_json_with_diagnostics(response, "jupiter get_orderbook").await.map_err(AdapterError::Parse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::venue::jupiter::JupEventMetadata;

    fn market(title: &str) -> JupMarket {
        JupMarket {
            market_id: "POLY-1".to_string(),
            title: title.to_string(),
            status: "open".to_string(),
            open_time: 1_000,
            close_time: 2_000,
            resolve_at: None,
            rules_primary: Some("  Resolves YES if the Dream win.  ".to_string()),
            pricing: None,
            ..Default::default()
        }
    }

    fn event(title: &str) -> JupEvent {
        JupEvent {
            event_id: "POLY-EVT-1".to_string(),
            category: "sports".to_string(),
            metadata: Some(JupEventMetadata {
                title: title.to_string(),
                subtitle: Some(String::new()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn the_parent_events_question_is_carried_alongside_the_outcome_label() {
        // The bug this guards: Jupiter puts the question on the event and only the outcome on the
        // market, so dropping the event rendered whole categories as "Up", "Down", "160-179".
        let converted = market_to_venue_market(Some(&event("Atlanta Dream vs New York Liberty")), &market("Atlanta Dream"));

        assert_eq!(converted.title, "Atlanta Dream");
        let carried = converted.event.expect("the parent event must be carried");
        assert_eq!(carried.title, "Atlanta Dream vs New York Liberty");
        assert_eq!(carried.event_id, "POLY-EVT-1");
    }

    #[test]
    fn resolution_rules_are_carried_and_trimmed() {
        let converted = market_to_venue_market(Some(&event("Anything")), &market("Yes"));
        assert_eq!(converted.rules.as_deref(), Some("Resolves YES if the Dream win."));
    }

    #[test]
    fn an_event_with_a_blank_title_is_not_carried() {
        // An empty title would render as a blank headline - worse than falling back to the
        // outcome label, which at least says something.
        let converted = market_to_venue_market(Some(&event("   ")), &market("Up"));
        assert!(converted.event.is_none());
    }

    #[test]
    fn a_blank_subtitle_is_dropped_rather_than_carried_as_empty() {
        let converted = market_to_venue_market(Some(&event("Real question")), &market("Up"));
        assert_eq!(converted.event.unwrap().subtitle, None);
    }

    #[test]
    fn a_market_with_no_event_still_converts() {
        // The single-market endpoint has no parent event to hand us.
        let converted = market_to_venue_market(None, &market("Standalone"));
        assert_eq!(converted.title, "Standalone");
        assert!(converted.event.is_none());
    }
}

