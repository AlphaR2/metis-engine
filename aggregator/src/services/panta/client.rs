//! Raw HTTP/wire layer for Panta's REST API. Pure request/response plumbing - no business logic
//! or `VenueAdapter` awareness here, that's `service.rs`. Wire types live in
//! `crate::models::venue::panta`.

use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::venue::panta::{PantaListResponse, PantaMarketRow, PantaQuoteResponse};
use crate::models::venue::{ListParams, MarketPage, VenueMarket, VenueQuote};
use crate::utils::http::parse_json_with_diagnostics;

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
/// Calls Panta's `GET /markets/` and returns one page of `primary`-phase markets.
pub async fn list_markets(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    params: ListParams,
) -> Result<MarketPage, AdapterError> {
    let mut query: Vec<(&str, String)> = vec![("limit", PAGE_LIMIT.to_string())];
    if let Some(category) = params.category
        && let Some(native) = crate::models::venue::panta::category_to_native(&category) {
            query.push(("category", native.to_string()));
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

    Ok(MarketPage { items: response.items.into_iter().map(Into::into).collect(), next_cursor: response.next_cursor })
}

/// Calls Panta's `GET /markets/{id}/` for one market.
#[allow(dead_code)] // adapter contract; no single-market route yet
pub async fn get_market(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    venue_market_id: &str,
) -> Result<VenueMarket, AdapterError> {
    let response = client
        .get(format!("{base_url}/markets/{venue_market_id}/"))
        .header("X-Api-Key", api_key)
        .send()
        .await?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(AdapterError::MarketNotFound(venue_market_id.to_string()));
    }

    let response = response.error_for_status().map_err(|e| AdapterError::Venue(e.to_string()))?;
    let row: PantaMarketRow =
        parse_json_with_diagnostics(response, "panta get_market").await.map_err(AdapterError::Parse)?;

    Ok(row.into())
}

/// Calls Panta's `POST /primaryorderquote/` for a fill estimate.
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

/// Raw GET passthrough — returns the venue's response body verbatim.
///
/// No parsing and no reshaping: the point is to see exactly what the venue sends, including the
/// fields we normally drop and the ones that arrive empty. Errors are returned as the body too,
/// since a venue's error text is usually the most informative thing about it.
pub async fn raw_get(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    path: &str,
    query: &[(String, String)],
) -> Result<String, AdapterError> {
    let path = path.trim_start_matches('/');
    let response = client
        .get(format!("{base_url}/{path}"))
        .header("X-Api-Key", api_key)
        .query(query)
        .send()
        .await?;
    response.text().await.map_err(AdapterError::Http)
}
