//! Wire-format data shapes for Jupiter Prediction's REST API -
//! https://developers.jup.ag/docs/api-reference/prediction. Field names taken directly from
//! Jupiter's own published OpenAPI spec, confirmed this session - corrects several earlier
//! guesses: this venue requires an `x-api-key` header (its spec's `security` block lists
//! `ApiKeyAuth` on every operation), pagination is offset-based (`start`/`end`/`hasNext`), not
//! cursor-based, and pricing is micro-USD integers, not decimal strings.

use rust_decimal::Decimal;
use serde::Deserialize;

use super::Category;
use super::timestamp::{deserialize_optional_timestamp, deserialize_timestamp};

/// Offset-based pagination info from `GET /events` - not a cursor token like every other venue.
/// `client.rs` encodes the next `start` index as a string so the shared `ListParams`/`MarketPage`
/// cursor contract still works unchanged from the caller's side.
#[derive(Debug, Deserialize)]
pub struct JupPagination {
    #[allow(dead_code)] // kept for Debug output; only `end`/`has_next` are read
    pub start: i64,
    pub end: i64,
    // Documented in Jupiter's OpenAPI spec, but real responses omit it entirely (confirmed live -
    // a real `/events` call returned `{"start":0,"end":10,"hasNext":true}`, no `total` at all).
    // Optional so a spec/reality mismatch on an unread field never breaks parsing of the fields we
    // actually need.
    #[allow(dead_code)]
    pub total: Option<i64>,
    #[serde(rename = "hasNext")]
    pub has_next: bool,
}

/// `GET /events` response envelope.
#[derive(Debug, Deserialize)]
pub struct JupEventsResponse {
    pub data: Vec<JupEvent>,
    pub pagination: JupPagination,
}

/// One event from `GET /events` - the category lives here, not on any individual market.
#[derive(Debug, Deserialize)]
pub struct JupEvent {
    #[serde(rename = "eventId")]
    pub event_id: String,
    pub category: String,
    /// Where the actual question lives. Each `JupMarket` below carries only its own outcome
    /// label, so without this a market reads as "Up", "160-179" or "September 21" - a fragment
    /// with nothing to attach it to.
    #[serde(default)]
    pub metadata: Option<JupEventMetadata>,
    #[serde(default)]
    pub markets: Vec<JupMarket>,
}

/// Event-level display data from `GET /events`.
#[derive(Debug, Deserialize, Clone)]
pub struct JupEventMetadata {
    /// The question, e.g. "What price will Bitcoin hit in 2026?".
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(rename = "imageUrl", default)]
    pub image_url: Option<String>,
}

/// Per-outcome pricing on a `JupMarket` - micro-USD integers (1_000_000 = $1.00), nullable until
/// the market has quotable prices.
#[derive(Debug, Deserialize, Clone)]
pub struct JupMarketPricing {
    #[serde(rename = "buyYesPriceUsd")]
    pub buy_yes_price_usd: Option<i64>,
    #[serde(rename = "buyNoPriceUsd")]
    pub buy_no_price_usd: Option<i64>,
    /// Traded volume for this market - the catalog-level depth signal. Present and non-zero on
    /// 248 of 250 open markets in a live sample, so it's a usable liquidity ranking key. Unlike
    /// the price fields this is a plain unit count, not micro-USD.
    #[serde(default)]
    pub volume: Option<f64>,
}

/// One binary market - fields are flat, no nested metadata, matching Jupiter's own OpenAPI
/// `Market` schema. Has no category of its own; that's an event-level property.
#[derive(Debug, Deserialize, Clone)]
pub struct JupMarket {
    #[serde(rename = "marketId")]
    pub market_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(rename = "openTime", deserialize_with = "deserialize_timestamp")]
    pub open_time: i64,
    /// When betting closes. This is *not* the resolution time - `resolveAt` is, and it is null
    /// while a market is still open (confirmed on all 250 open markets in a live sample). An
    /// earlier version of this integration mapped `closeTime` straight onto `resolution_time`,
    /// which conflated two genuinely different moments.
    #[serde(rename = "closeTime", deserialize_with = "deserialize_timestamp")]
    pub close_time: i64,
    /// When the outcome is settled - null until Jupiter knows it.
    #[serde(rename = "resolveAt", default, deserialize_with = "deserialize_optional_timestamp")]
    pub resolve_at: Option<i64>,
    /// The full resolution terms - what counts as a win and from which source. This is the text a
    /// trader most needs before committing, and it was previously dropped on the floor.
    #[serde(rename = "rulesPrimary", default)]
    pub rules_primary: Option<String>,
    pub pricing: Option<JupMarketPricing>,
}

/// The categories Jupiter's `category` filter accepts - it 400s on anything else, so this is a
/// hard allow-list, not a hint. Confirmed live from its own rejection message: `all|crypto|
/// sports|politics|esports|culture|economics|tech|finance|climate & science|weather|mentions`.
/// Returns `None` for a canonical category Jupiter has no equivalent for.
pub fn category_to_native(category: &Category) -> Option<&'static str> {
    match category {
        Category::Crypto => Some("crypto"),
        Category::Sports => Some("sports"),
        Category::Politics => Some("politics"),
        Category::Esports => Some("esports"),
        Category::Culture => Some("culture"),
        Category::Economics => Some("economics"),
        Category::Technology => Some("tech"),
        Category::Finance => Some("finance"),
        Category::Science => Some("climate & science"),
        Category::Weather => Some("weather"),
        // Jupiter has no entertainment/world bucket.
        _ => None,
    }
}

/// `GET /orderbook/{id}` response - only the decimal-priced arrays are used; the integer-cents
/// `yes`/`no` arrays are redundant with these (same data, coarser precision) and skipped.
#[derive(Debug, Deserialize)]
pub struct JupOrderbookResponse {
    /// `[price_decimal_string, quantity]` pairs - quantity is a raw JSON number (may be
    /// fractional), unlike the price, which is a decimal string.
    pub yes_dollars: Vec<(String, f64)>,
    pub no_dollars: Vec<(String, f64)>,
}

/// Converts a micro-USD integer (1_000_000 = $1.00) into a human decimal string via exact
/// integer-scaled `Decimal` division - never through `f64`, so no float rounding error ever
/// touches a price.
pub fn micro_usd_to_decimal_string(micro: i64) -> String {
    (Decimal::from(micro) / Decimal::from(1_000_000)).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_micro_usd_exactly() {
        assert_eq!(micro_usd_to_decimal_string(650_000), "0.65");
        assert_eq!(micro_usd_to_decimal_string(10_000), "0.01");
        assert_eq!(micro_usd_to_decimal_string(1_000_000), "1");
    }
}

// ── Order placement ─────────────────────────────────────────────────────────────────────────
//
// `POST /orders` builds an unsigned transaction; the caller signs it and submits it to Solana
// themselves. There is **no execute endpoint** — an earlier note in this integration claimed
// `POST /execute` existed, which was wrong. Status is then polled from
// `GET /orders/status/{orderPubkey}`.
//
// Money is micro-USD (1_000_000 = $1.00), as everywhere else in Jupiter's API.

use serde::Serialize;

/// `GET /trading-status` — must be checked before building an order.
#[derive(Debug, Deserialize)]
pub struct JupTradingStatus {
    /// Accepts either spelling: the docs use snake_case here while the rest of the API is camel.
    #[serde(alias = "tradingActive", default)]
    pub trading_active: bool,
}

/// `POST /orders` request, buy side.
#[derive(Debug, Serialize)]
pub struct JupCreateOrderRequest {
    #[serde(rename = "ownerPubkey")]
    pub owner_pubkey: String,
    #[serde(rename = "marketId")]
    pub market_id: String,
    /// Which side: true for YES, false for NO.
    #[serde(rename = "isYes")]
    pub is_yes: bool,
    #[serde(rename = "isBuy")]
    pub is_buy: bool,
    /// Capital to commit, in the deposit mint's base units (USDC has 6 decimals).
    #[serde(rename = "depositAmount")]
    pub deposit_amount: String,
    #[serde(rename = "depositMint", skip_serializing_if = "Option::is_none")]
    pub deposit_mint: Option<String>,
}

/// Blockhash metadata from an order build.
#[derive(Debug, Deserialize)]
pub struct JupTxMeta {
    #[serde(rename = "lastValidBlockHeight", default)]
    pub last_valid_block_height: Option<u64>,
}

/// The economics of a built order.
///
/// Jupiter computes these; none of them can be supplied on the request. That matters for
/// `maxBuyPriceUsd` in particular - it is a real price ceiling encoded in the transaction, but it
/// is **Jupiter's** ceiling, not a tolerance the caller chose.
#[derive(Debug, Deserialize)]
pub struct JupOrderDetail {
    #[serde(rename = "orderPubkey")]
    pub order_pubkey: String,
    /// Contracts acquired, as a decimal string.
    #[serde(rename = "contractsDecimal", default)]
    pub contracts_decimal: Option<String>,
    /// The worst per-contract price this order can fill at, micro-USD.
    #[serde(rename = "maxBuyPriceUsd", default)]
    pub max_buy_price_usd: Option<i64>,
    /// What the order costs in total, micro-USD.
    #[serde(rename = "orderCostUsd", default)]
    pub order_cost_usd: Option<i64>,
    #[serde(rename = "estimatedProtocolFeeUsd", default)]
    pub estimated_protocol_fee_usd: Option<i64>,
    #[serde(rename = "estimatedVenueFeeUsd", default)]
    pub estimated_venue_fee_usd: Option<i64>,
    /// The sum of the two above. Captured so the response shape is complete, but not shown:
    /// the breakdown is itemised, and a total line beside its own parts is noise.
    #[allow(dead_code)]
    #[serde(rename = "estimatedTotalFeeUsd", default)]
    pub estimated_total_fee_usd: Option<i64>,
    /// Expected slippage, and the ceiling Jupiter will tolerate, in basis points.
    #[serde(rename = "slippageBps", default)]
    pub slippage_bps: Option<i64>,
    #[serde(rename = "maxSlippageBps", default)]
    pub max_slippage_bps: Option<i64>,
    /// The average price the resulting position would carry, micro-USD.
    #[serde(rename = "newAvgPriceUsd", default)]
    pub new_avg_price_usd: Option<i64>,
    /// What the position pays out if it wins, micro-USD.
    #[serde(rename = "newPayoutUsd", default)]
    pub new_payout_usd: Option<i64>,
}

/// `POST /orders` response.
#[derive(Debug, Deserialize)]
pub struct JupCreateOrderResponse {
    /// Base64 unsigned transaction.
    pub transaction: String,
    #[serde(rename = "txMeta", default)]
    pub tx_meta: Option<JupTxMeta>,
    #[serde(rename = "requiredSigners", default)]
    pub required_signers: Vec<String>,
    pub order: JupOrderDetail,
}

/// `GET /orders/status/{orderPubkey}` response.
#[derive(Debug, Deserialize)]
pub struct JupOrderStatus {
    /// `created`, `partiallyfilled`, `filled` or `failed`.
    #[serde(default)]
    pub status: String,
    #[serde(rename = "marketId", default)]
    pub market_id: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

/// Converts a micro-USD integer into decimal USD, exactly.
pub fn micro_usd_to_decimal(micro: i64) -> Decimal {
    Decimal::from(micro) / Decimal::from(1_000_000)
}

/// Converts decimal USDC into the base units `depositAmount` expects.
///
/// USDC carries 6 decimals, so this is a scale-and-truncate. Truncating rather than rounding up
/// matters: rounding up would spend fractionally more of the user's money than they asked to.
pub fn usdc_to_base_units(amount: Decimal) -> i64 {
    (amount * Decimal::from(1_000_000)).trunc().try_into().unwrap_or(0)
}
