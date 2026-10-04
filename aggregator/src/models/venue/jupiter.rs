//! Wire-format data shapes for Jupiter Prediction's REST API -
//! https://developers.jup.ag/docs/api-reference/prediction. Field names taken directly from
//! Jupiter's own published OpenAPI spec, confirmed this session - corrects several earlier
//! guesses: this venue requires an `x-api-key` header (its spec's `security` block lists
//! `ApiKeyAuth` on every operation), pagination is offset-based (`start`/`end`/`hasNext`), not
//! cursor-based, and pricing is micro-USD integers, not decimal strings.

use rust_decimal::Decimal;
use serde::Deserialize;

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
///
/// Fields confirmed in the Phase 0 capture (`docs/venue-fields.md`). Anything not modelled here
/// lands in `extra`, so nothing Jupiter sends is dropped.
#[derive(Debug, Deserialize, Default)]
pub struct JupEvent {
    #[serde(rename = "eventId")]
    pub event_id: String,
    pub category: String,
    /// League or asset class, e.g. `mlb`, `cs2`, `btc`. Empty on about half of events.
    #[serde(default)]
    pub subcategory: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Total volume, micro-USD as a string.
    #[serde(rename = "volumeUsd", default)]
    pub volume_usd: Option<String>,
    /// Last-24h volume, micro-USD as a string.
    #[serde(rename = "volume24hr", default)]
    pub volume_24hr: Option<String>,
    /// When the real-world event starts - Unix seconds as a fractional string.
    #[serde(rename = "beginAt", default, deserialize_with = "deserialize_optional_timestamp")]
    pub begin_at: Option<i64>,
    #[serde(rename = "closeCondition", default)]
    pub close_condition: Option<String>,
    /// Where the actual question lives. Each `JupMarket` below carries only its own outcome
    /// label, so without this a market reads as "Up", "160-179" or "September 21" - a fragment
    /// with nothing to attach it to.
    #[serde(default)]
    pub metadata: Option<JupEventMetadata>,
    #[serde(default)]
    pub markets: Vec<JupMarket>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Event-level display data from `GET /events`.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct JupEventMetadata {
    /// The question, e.g. "What price will Bitcoin hit in 2026?".
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(rename = "imageUrl", default)]
    pub image_url: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    /// Competition name, e.g. "Dota 2". Set on about one event in ten.
    #[serde(default)]
    pub league: Option<String>,
}

/// A team attached to a market (`isTeamMarket`).
#[derive(Debug, Deserialize, Clone)]
pub struct JupTeam {
    pub name: String,
    #[serde(default)]
    pub abbreviation: Option<String>,
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
#[derive(Debug, Deserialize, Clone, Default)]
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
    /// Where Jupiter sources the market: `polymarket` or `kalshi`.
    #[serde(default)]
    pub provider: Option<String>,
    /// Outcome labels, e.g. `["Falcons", "Packers"]` - both sides of a matchup.
    #[serde(default)]
    pub outcomes: Vec<String>,
    #[serde(default)]
    pub team: Option<JupTeam>,
    /// `moneyline`, … - set on sports markets only.
    #[serde(rename = "sportsMarketType", default)]
    pub sports_market_type: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The categories Jupiter's `category` filter accepts - it 400s on anything else, and it has no
/// endpoint that lists them (`GET /categories` is a 404), so this is the one place that knowledge
/// lives. Confirmed live from its own rejection message; `all` is omitted because the refresh's
/// unfiltered sweep already covers it. A category Jupiter adds is still picked up by that sweep,
/// bucketed under its own name, before this list is updated.
pub const FILTER_CATEGORIES: [&str; 11] = [
    "crypto", "sports", "politics", "esports", "culture", "economics", "tech", "finance", "climate & science",
    "weather", "mentions",
];

/// `GET /orderbook/{id}` response - only the decimal-priced arrays are used; the integer-cents
/// `yes`/`no` arrays are redundant with these (same data, coarser precision) and skipped.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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

/// An integer Jupiter may send as a JSON number or as a string of digits ("210000"), or as null.
///
/// An order response mixes the two - `contractsMicro` is a string, `maxBuyPriceUsd` can be either -
/// and a strict `i64` turns the first string into a refused order. Read both, so a formatting quirk
/// on Jupiter's side cannot block a build that is otherwise fine.
fn lenient_i64<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Number {
        Int(i64),
        Float(f64),
        Text(String),
    }
    match Option::<Number>::deserialize(deserializer)? {
        None => Ok(None),
        Some(Number::Int(n)) => Ok(Some(n)),
        Some(Number::Float(f)) if f.is_finite() => Ok(Some(f as i64)),
        Some(Number::Float(f)) => Err(serde::de::Error::custom(format!("{f} is not a finite number"))),
        Some(Number::Text(t)) => {
            let t = t.trim();
            if t.is_empty() {
                return Ok(None);
            }
            t.parse::<i64>()
                .or_else(|_| t.parse::<f64>().map(|f| f as i64))
                .map(Some)
                .map_err(|_| serde::de::Error::custom(format!("'{t}' is not a number")))
        }
    }
}

fn lenient_u64<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    Ok(lenient_i64(deserializer)?.and_then(|n| u64::try_from(n).ok()))
}

/// Blockhash metadata from an order build.
#[derive(Debug, Deserialize)]
pub struct JupTxMeta {
    #[serde(rename = "lastValidBlockHeight", default, deserialize_with = "lenient_u64")]
    pub last_valid_block_height: Option<u64>,
}

/// The economics of a built order.
///
/// Jupiter computes these; none of them can be supplied on the request. That matters for
/// `maxBuyPriceUsd` in particular - it is a real price ceiling encoded in the transaction, but it
/// is **Jupiter's** ceiling, not a tolerance the caller chose.
#[derive(Debug, Deserialize)]
pub struct JupOrderDetail {
    /// Null when Jupiter priced the order but could not build it (see `JupCreateOrderResponse`).
    #[serde(rename = "orderPubkey", default)]
    pub order_pubkey: Option<String>,
    /// Contracts acquired, as a decimal string.
    #[serde(rename = "contractsDecimal", default)]
    pub contracts_decimal: Option<String>,
    /// The worst per-contract price this order can fill at, micro-USD.
    #[serde(rename = "maxBuyPriceUsd", default, deserialize_with = "lenient_i64")]
    pub max_buy_price_usd: Option<i64>,
    /// What the order costs in total, micro-USD.
    #[serde(rename = "orderCostUsd", default, deserialize_with = "lenient_i64")]
    pub order_cost_usd: Option<i64>,
    #[serde(rename = "estimatedProtocolFeeUsd", default, deserialize_with = "lenient_i64")]
    pub estimated_protocol_fee_usd: Option<i64>,
    #[serde(rename = "estimatedVenueFeeUsd", default, deserialize_with = "lenient_i64")]
    pub estimated_venue_fee_usd: Option<i64>,
    /// The sum of the two above. Captured so the response shape is complete, but not shown:
    /// the breakdown is itemised, and a total line beside its own parts is noise.
    #[allow(dead_code)]
    #[serde(rename = "estimatedTotalFeeUsd", default, deserialize_with = "lenient_i64")]
    pub estimated_total_fee_usd: Option<i64>,
    /// Expected slippage, and the ceiling Jupiter will tolerate, in basis points.
    #[serde(rename = "slippageBps", default, deserialize_with = "lenient_i64")]
    pub slippage_bps: Option<i64>,
    #[serde(rename = "maxSlippageBps", default, deserialize_with = "lenient_i64")]
    pub max_slippage_bps: Option<i64>,
    /// The average price the resulting position would carry, micro-USD.
    #[serde(rename = "newAvgPriceUsd", default, deserialize_with = "lenient_i64")]
    pub new_avg_price_usd: Option<i64>,
    /// What the position pays out if it wins, micro-USD.
    #[serde(rename = "newPayoutUsd", default, deserialize_with = "lenient_i64")]
    pub new_payout_usd: Option<i64>,
}

/// `POST /orders` response.
#[derive(Debug, Deserialize)]
pub struct JupCreateOrderResponse {
    /// Base64 unsigned transaction. Null when Jupiter priced the order but built nothing - seen when
    /// the wallet cannot fund it - in which case the order pubkey is null too.
    #[serde(default)]
    pub transaction: Option<String>,
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

// ── Positions, claims ───────────────────────────────────────────────────────────────────────
//
// Field names from Jupiter's published OpenAPI spec (`GET /positions`, `POST
// /positions/{positionPubkey}/claim`). The spec types the USD fields as strings while the
// documentation elsewhere says integers, so every micro-USD field here accepts a string, an
// integer or a float rather than trusting either.

/// Micro-USD (1_000_000 = $1.00) sent as a string, an integer, a float or null -> dollars.
fn deserialize_micro_usd<'de, D>(deserializer: D) -> Result<Option<Decimal>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    let micro = match raw {
        Some(serde_json::Value::String(text)) => text.trim().parse::<Decimal>().ok(),
        Some(serde_json::Value::Number(number)) => number.to_string().parse::<Decimal>().ok(),
        _ => None,
    };
    Ok(micro.map(|m| m / Decimal::from(1_000_000)))
}

/// `GET /positions` response envelope.
#[derive(Debug, Deserialize)]
pub struct JupPositionsResponse {
    #[serde(default)]
    pub data: Vec<JupPosition>,
    pub pagination: JupPagination,
}

/// One position. Only the fields the API reports on are modelled; the rest of what Jupiter sends is
/// dropped here because nothing reads it.
#[derive(Debug, Deserialize, Default)]
pub struct JupPosition {
    /// The position account - what a claim or a sell is keyed on.
    pub pubkey: String,
    #[serde(rename = "ownerPubkey", default)]
    pub owner_pubkey: Option<String>,
    #[serde(rename = "marketId")]
    pub market_id: String,
    #[serde(rename = "isYes", default)]
    pub is_yes: bool,
    #[serde(rename = "contractsDecimal", default)]
    pub contracts_decimal: Option<String>,
    #[serde(rename = "contractsMicro", default)]
    pub contracts_micro: Option<String>,
    #[serde(rename = "totalCostUsd", default, deserialize_with = "deserialize_micro_usd")]
    pub total_cost: Option<Decimal>,
    #[serde(rename = "avgPriceUsd", default, deserialize_with = "deserialize_micro_usd")]
    pub avg_price: Option<Decimal>,
    #[serde(rename = "valueUsd", default, deserialize_with = "deserialize_micro_usd")]
    pub value: Option<Decimal>,
    #[serde(rename = "pnlUsd", default, deserialize_with = "deserialize_micro_usd")]
    pub pnl: Option<Decimal>,
    #[serde(rename = "payoutUsd", default, deserialize_with = "deserialize_micro_usd")]
    pub payout: Option<Decimal>,
    /// Orders placed on this position that have not filled yet.
    #[serde(rename = "openOrders", default)]
    pub open_orders: u32,
    #[serde(default)]
    pub claimed: bool,
    #[serde(default)]
    pub claimable: bool,
    #[serde(rename = "updatedAt", default, deserialize_with = "deserialize_optional_timestamp")]
    pub updated_at: Option<i64>,
    /// When the market settled, if it has.
    #[serde(rename = "settlementDate", default, deserialize_with = "deserialize_optional_timestamp")]
    pub settlement_date: Option<i64>,
    #[serde(rename = "marketMetadata", default)]
    pub market_metadata: Option<serde_json::Value>,
    #[serde(rename = "eventMetadata", default)]
    pub event_metadata: Option<serde_json::Value>,
}

impl JupPosition {
    /// Shares held. `contractsDecimal` is the exact figure; the micro count is the fallback.
    pub fn contracts(&self) -> Decimal {
        self.contracts_decimal
            .as_deref()
            .and_then(|c| c.trim().parse::<Decimal>().ok())
            .or_else(|| {
                self.contracts_micro
                    .as_deref()
                    .and_then(|c| c.trim().parse::<Decimal>().ok())
                    .map(|m| m / Decimal::from(1_000_000))
            })
            .unwrap_or_default()
    }

    /// The market's title from whichever metadata block carries one.
    pub fn title(&self) -> Option<String> {
        [&self.market_metadata, &self.event_metadata].into_iter().flatten().find_map(|meta| {
            ["title", "name"].iter().find_map(|key| meta.get(*key)?.as_str().map(str::to_string))
        })
    }
}

/// `POST /positions/{positionPubkey}/claim` response.
#[derive(Debug, Deserialize)]
pub struct JupClaimResponse {
    /// Base64 unsigned transaction.
    pub transaction: String,
    #[serde(rename = "txMeta", default)]
    pub tx_meta: Option<JupTxMeta>,
    #[serde(default)]
    pub position: Option<JupClaimPosition>,
}

#[derive(Debug, Deserialize, Default)]
pub struct JupClaimPosition {
    #[serde(rename = "contractsDecimal", default)]
    pub contracts_decimal: Option<String>,
    #[serde(rename = "payoutAmountUsd", default, deserialize_with = "deserialize_micro_usd")]
    pub payout: Option<Decimal>,
}

#[cfg(test)]
mod position_tests {
    use super::*;

    #[test]
    fn usd_fields_accept_a_string_an_integer_a_float_or_null() {
        let position: JupPosition = serde_json::from_str(
            r#"{"pubkey":"P","marketId":"M","isYes":true,
                "totalCostUsd":"5000000","avgPriceUsd":520000,"valueUsd":4900000.0,"pnlUsd":null,
                "contractsDecimal":"9.615384","claimable":false,"claimed":false,"openOrders":1}"#,
        )
        .unwrap();
        assert_eq!(position.total_cost, Some(Decimal::from(5)));
        assert_eq!(position.avg_price, Some("0.52".parse().unwrap()));
        assert_eq!(position.value, Some("4.9".parse().unwrap()));
        assert_eq!(position.pnl, None, "null is unknown, never zero");
        assert_eq!(position.contracts(), "9.615384".parse::<Decimal>().unwrap());
        assert_eq!(position.open_orders, 1);
    }

    #[test]
    fn contracts_fall_back_to_the_micro_count() {
        let position: JupPosition =
            serde_json::from_str(r#"{"pubkey":"P","marketId":"M","contractsMicro":"2500000"}"#).unwrap();
        assert_eq!(position.contracts(), "2.5".parse::<Decimal>().unwrap());
    }

    #[test]
    fn the_title_comes_from_whichever_metadata_has_one() {
        let position: JupPosition = serde_json::from_str(
            r#"{"pubkey":"P","marketId":"M","marketMetadata":{"title":"Will it rain?"},"eventMetadata":{"title":"Weather"}}"#,
        )
        .unwrap();
        assert_eq!(position.title().as_deref(), Some("Will it rain?"));
    }

    #[test]
    fn a_position_with_no_optional_fields_still_parses() {
        assert!(serde_json::from_str::<JupPosition>(r#"{"pubkey":"P","marketId":"M"}"#).is_ok());
    }
}

#[cfg(test)]
mod order_response_tests {
    use super::*;

    #[test]
    fn an_order_whose_numbers_are_sometimes_strings_still_parses() {
        // As Jupiter sent it: a real transaction, with some micro-USD fields as strings ("210000")
        // and others as numbers, block height as a string.
        let body = r#"{"transaction":"AAAA","txMeta":{"lastValidBlockHeight":"431328938"},
            "requiredSigners":["W"],"isGasless":false,
            "order":{"orderPubkey":"O1","contractsDecimal":"4.7","maxBuyPriceUsd":"210000","orderCostUsd":2000000,
                     "estimatedProtocolFeeUsd":"0","estimatedVenueFeeUsd":null,"slippageBps":"15","newAvgPriceUsd":425000.0,
                     "newPayoutUsd":""}}"#;
        let built: JupCreateOrderResponse = serde_json::from_str(body).expect("mixed numbers must parse");
        assert_eq!(built.order.max_buy_price_usd, Some(210_000));
        assert_eq!(built.order.order_cost_usd, Some(2_000_000));
        assert_eq!(built.order.estimated_venue_fee_usd, None);
        assert_eq!(built.order.slippage_bps, Some(15));
        assert_eq!(built.order.new_avg_price_usd, Some(425_000));
        assert_eq!(built.order.new_payout_usd, None, "an empty string is no value, not an error");
        assert_eq!(built.tx_meta.unwrap().last_valid_block_height, Some(431_328_938));
    }

    #[test]
    fn a_value_that_is_not_a_number_is_still_an_error() {
        let body = r#"{"transaction":"AAAA","order":{"orderPubkey":"O1","maxBuyPriceUsd":"abc"}}"#;
        assert!(serde_json::from_str::<JupCreateOrderResponse>(body).is_err());
    }
}
