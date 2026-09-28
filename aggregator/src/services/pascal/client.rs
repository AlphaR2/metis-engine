//! Raw HTTP/wire layer for Pascal's REST API. Pure request/response plumbing - no business logic
//! (long-vs-short book selection and price inversion are trading decisions, made in
//! `service.rs`). Wire types live in `crate::models::venue::pascal`.

use crate::errors::AdapterError;
use crate::models::venue::pascal::{PascalBook, PascalBooksResponse, PascalMarket, PascalMarketsResponse};
use crate::models::venue::{Category, MarketPage, VenueMarket};
use crate::utils::http::parse_json_with_diagnostics;

/// Converts a millisecond timestamp string to whole Unix seconds.
fn ms_to_secs(ms: &str) -> i64 {
    ms.parse::<i64>().unwrap_or(0) / 1000
}

/// Flattens a Pascal market into a venue-agnostic `VenueMarket`.
fn market_to_venue_market(market: PascalMarket) -> VenueMarket {
    let category = market.display_attributes.tags.first().cloned().unwrap_or_default();
    VenueMarket {
        venue_id: "pascal".to_string(),
        venue_market_id: market.symbol,
        title: market.display_attributes.event_description,
        // Pascal has no event layer of its own.
        event: None,
        rules: None,
        category: Category::from(category),
        // Pascal has no single "phase" string like Panta's primary/secondary - the market's own
        // description already carries the outcome being quoted (e.g. "Yes, Team A wins").
        phase: market.display_attributes.market_description,
        start_time: market
            .display_attributes
            .expected_event_start_time_ms
            .as_deref()
            .map(ms_to_secs)
            .unwrap_or(0),
        // Pascal documents no separate betting-cutoff field, so close falls back to resolution.
        // Unlike Panta's `endTime` and Jupiter's `closeTime` this is *not* a venue-reported
        // cutoff - it is the honest best available, and callers filtering a close window on
        // Pascal are really filtering on resolution.
        close_time: ms_to_secs(&market.display_attributes.expected_resolution_time_ms),
        resolution_time: ms_to_secs(&market.display_attributes.expected_resolution_time_ms),
        // Pascal has no separate yes/no price - mark_price is the single settlement-price
        // estimate for "long" here; "short" is derived as 1 - mark_price by the canonical layer,
        // not duplicated here as a second field.
        yes_price: market.mark_price,
        no_price: None,
        // Pascal's /markets rows carry no volume/depth field.
        liquidity: None,
        // Pascal's real tradeability signal is unconfirmed against docs this session - default to
        // not-dead rather than silently hiding every Pascal market until that's verified.
        dead: false,
        ..Default::default()
    }
}

/// Calls Pascal's `GET /markets` for the full market list - no pagination/category filters
/// documented, so `ListParams` is accepted for trait-shape consistency but unused here.
pub async fn list_markets(client: &reqwest::Client, base_url: &str) -> Result<MarketPage, AdapterError> {
    let response = client
        .get(format!("{base_url}/markets"))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AdapterError::Venue(e.to_string()))?;
    let response: PascalMarketsResponse =
        parse_json_with_diagnostics(response, "pascal list_markets").await.map_err(AdapterError::Parse)?;

    Ok(MarketPage {
        items: response.markets.into_iter().map(market_to_venue_market).collect(),
        next_cursor: None,
    })
}

/// Finds one market by symbol - no single-market-by-symbol GET is documented, so this filters the
/// full list instead. Fine at MVP scale (a curated venue network), worth revisiting if Pascal's
/// market count grows large enough to make this wasteful.
#[allow(dead_code)] // adapter contract; no single-market route yet
pub async fn get_market(client: &reqwest::Client, base_url: &str, venue_market_id: &str) -> Result<VenueMarket, AdapterError> {
    let page = list_markets(client, base_url).await?;
    page.items
        .into_iter()
        .find(|m| m.venue_market_id == venue_market_id)
        .ok_or_else(|| AdapterError::MarketNotFound(venue_market_id.to_string()))
}

/// Calls Pascal's `GET /books` and returns the raw book for one symbol. Which side to walk and
/// how (long vs short, price inversion) is a trading decision - that's `service.rs`'s job.
pub async fn get_book(client: &reqwest::Client, base_url: &str, venue_market_id: &str) -> Result<PascalBook, AdapterError> {
    let response = client
        .get(format!("{base_url}/books"))
        .query(&[("symbols", venue_market_id)])
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AdapterError::Venue(e.to_string()))?;
    let mut response: PascalBooksResponse =
        parse_json_with_diagnostics(response, "pascal get_book").await.map_err(AdapterError::Parse)?;

    response
        .books
        .remove(venue_market_id)
        .ok_or_else(|| AdapterError::MarketNotFound(venue_market_id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_to_secs_converts_millisecond_timestamps() {
        assert_eq!(ms_to_secs("1_700_000_000_000".replace('_', "").as_str()), 1_700_000_000);
    }
}
