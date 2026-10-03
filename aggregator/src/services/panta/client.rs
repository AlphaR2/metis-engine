//! Raw HTTP/wire layer for Panta's REST API. Pure request/response plumbing - no business logic
//! or `VenueAdapter` awareness here, that's `service.rs`. Wire types live in
//! `crate::models::venue::panta`.

use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::venue::panta::{PantaCategoriesResponse, PantaListResponse, PantaMarketRow, PantaQuoteResponse};
use crate::models::venue::{ListParams, VenueQuote};
use crate::utils::http::{parse_json_with_diagnostics, venue_url_dir};

/// Panta's documented max page size (default is 20 if unset) - requesting the max cuts the number
/// of round trips (and thus total refresh time) roughly 2.5x for the same catalog.
const PAGE_LIMIT: &str = "50";

/// Panta's list endpoint ignores every filter param we send it - `status`, `phase`, `resolved`
/// and any sort key alike. Verified live: two identical `status=primary` calls returned different
/// phase mixes, and the first page is byte-identical whatever params are attached. The `status`
/// filter this client used to send was doing nothing, so it is gone rather than left in place
/// looking load-bearing.
///
/// What *is* honoured is `category` (confirmed: sports/crypto/politics each return only their own
/// rows) and `limit`. Everything else - dropping resolved/cancelled/secondary rows, the betting
/// window - is enforced client-side by `VenueMarket::is_bettable`.
/// Calls Panta's `GET /markets/` and returns one page of raw rows and the next cursor.
pub async fn list_markets(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    params: ListParams,
) -> Result<(Vec<PantaMarketRow>, Option<String>), AdapterError> {
    let mut query: Vec<(&str, String)> = vec![("limit", PAGE_LIMIT.to_string())];
    if let Some(category) = params.category {
        // Panta's own spelling, as its `/categories/` returned it.
        query.push(("category", category));
    }
    if let Some(cursor) = params.cursor {
        query.push(("cursor", cursor));
    }

    let response = client
        .get(format!("{base_url}/markets/"))
        .header("X-Api-Key", api_key)
        .query(&query)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AdapterError::Venue(e.to_string()))?;

    let response: PantaListResponse = parse_json_with_diagnostics(response, "panta list_markets")
        .await
        .map_err(AdapterError::Parse)?;

    Ok((response.items, response.next_cursor))
}

/// Calls Panta's `GET /categories/` - the names its `category` filter accepts.
pub async fn list_categories(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<String>, AdapterError> {
    let response = client
        .get(venue_url_dir(base_url, &["categories"])?)
        .header("X-Api-Key", api_key)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AdapterError::Venue(e.to_string()))?;
    let response: PantaCategoriesResponse = parse_json_with_diagnostics(response, "panta list_categories")
        .await
        .map_err(AdapterError::Parse)?;
    Ok(response.categories)
}

/// Calls Panta's `GET /markets/{id}/` for one market - the only place its question, rules and
/// sources appear.
pub async fn get_market_row(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    venue_market_id: &str,
) -> Result<PantaMarketRow, AdapterError> {
    let response = client
        .get(venue_url_dir(base_url, &["markets", venue_market_id])?)
        .header("X-Api-Key", api_key)
        .send()
        .await?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(AdapterError::MarketNotFound(venue_market_id.to_string()));
    }

    let response = response.error_for_status().map_err(|e| AdapterError::Venue(e.to_string()))?;
    parse_json_with_diagnostics(response, "panta get_market").await.map_err(AdapterError::Parse)
}

/// Calls Panta's `POST /primaryorderquote/` for a fill estimate.
#[allow(dead_code)]
pub async fn quote(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    quote_wallet: &str,
    venue_market_id: &str,
    side: &str,
    amount_usdc: Decimal,
) -> Result<VenueQuote, AdapterError> {
    let body = serde_json::json!({
        "wallet": quote_wallet,
        "marketId": venue_market_id,
        "side": side,
        "amountUsdc": amount_usdc.to_string(),
    });

    let response = client
        .post(format!("{base_url}/primaryorderquote/"))
        .header("X-Api-Key", api_key)
        .json(&body)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AdapterError::Venue(e.to_string()))?;
    let response: PantaQuoteResponse =
        parse_json_with_diagnostics(response, "panta quote").await.map_err(AdapterError::Parse)?;

    let parse_field = |name: &'static str, raw: &str| -> Result<Decimal, AdapterError> {
        raw.parse().map_err(|_| AdapterError::Parse(format!("panta quote field '{name}' = '{raw}' is not a decimal")))
    };

    Ok(VenueQuote {
        venue: "panta",
        venue_market_id: venue_market_id.to_string(),
        side: side.to_string(),
        amount_usdc,
        effective_shares_or_size: parse_field("shares", &response.shares)?,
        avg_price: parse_field("avgPrice", &response.avg_price)?,
        fee_usdc: parse_field("feeUsdc", &response.fee_usdc)?,
        // A bonding curve always fills the full amount - there is no book to run out of.
        fully_filled: true,
        expires_at: Some(response.expires_at),
    })
}

