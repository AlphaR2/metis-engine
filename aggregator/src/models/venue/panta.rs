//! Wire-format data shapes for Panta's REST API - https://docs.panta.market. Field names taken
//! directly from their published API reference, confirmed this session.
//!
//! `startTime`/`resolutionTime` are documented as Unix-second integers, and that's what real
//! live-market data actually returns - but a sandbox (`pk_test_`) response returned ISO-8601
//! strings instead (e.g. `"2026-01-01T00:00:00Z"`) for the same fields. Confirmed directly
//! against both the live and sandbox API, not a docs typo - the wire format genuinely differs
//! between environments. Deserialized to accept either shape rather than assuming one.

use serde::Deserialize;

use super::timestamp::{deserialize_optional_timestamp, deserialize_timestamp};
use super::{Category, VenueMarket};

/// The categories Panta's `category` filter actually accepts, confirmed live against its own
/// `GET /categories/`. Returns `None` for a canonical category Panta has no equivalent for, so a
/// caller filtering by e.g. `esports` simply gets no Panta markets instead of a 400 from Panta.
pub fn category_to_native(category: &Category) -> Option<&'static str> {
    match category {
        Category::Sports => Some("sports"),
        Category::Crypto => Some("crypto"),
        Category::Politics => Some("politics"),
        Category::Entertainment => Some("entertainment"),
        Category::Finance => Some("finance"),
        Category::Science => Some("science"),
        Category::World => Some("world"),
        Category::Other => Some("other"),
        // Panta has no esports/culture/economics/tech/weather bucket of its own, and `Unknown`
        // is by definition something we could not place.
        _ => None,
    }
}

/// The canonical categories Panta accepts, derived from `category_to_native` itself.
///
/// Derived rather than written out, because the hand-written version of this list drifted from the
/// mapping and produced an error message that rejected `other` while listing `other` as accepted.
/// A message that contradicts the code is worse than no message.
pub fn supported_categories() -> Vec<String> {
    Category::all()
        .into_iter()
        .filter(|c| category_to_native(c).is_some())
        .map(String::from)
        .collect()
}

/// One row of Panta's `/markets/` list response.
///
/// `phase` and `status` are two genuinely separate wire fields - `phase` is the trading stage
/// (`primary`/`secondary`/`resolved`/`cancelled`, the same enum our own `status` query param
/// filters on) while `status` is Panta's own tradeability flag (`open`/`resolved`/`cancelled`).
/// Real data confirmed these can disagree (e.g. a market still `phase: "primary"` but already
/// `status: "cancelled"`) - both are captured so `VenueMarket::dead` can catch either signal.
#[derive(Debug, Deserialize)]
pub struct PantaMarketRow {
    #[serde(rename = "marketId")]
    pub market_id: String,
    /// Empty on every row of a 300-row live sample - Panta populates `description` instead, and
    /// often not even that. `From<PantaMarketRow>` falls back accordingly.
    #[serde(default)]
    pub title: String,
    /// Panta usually leaves `title` empty and puts the market's question here instead.
    #[serde(default)]
    pub description: String,
    pub category: String,
    pub phase: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub resolved: bool,
    #[serde(rename = "startTime", deserialize_with = "deserialize_timestamp")]
    pub start_time: i64,
    /// When betting closes - a genuinely separate field from `resolutionTime`, which is when the
    /// outcome is known. Confirmed live: they differed on 38 of 50 sampled markets, by anything
    /// from hours to five days. Optional because it is absent from some older rows; the conversion
    /// falls back to `resolutionTime` when it's missing.
    #[serde(rename = "endTime", default, deserialize_with = "deserialize_optional_timestamp")]
    pub end_time: Option<i64>,
    #[serde(rename = "resolutionTime", deserialize_with = "deserialize_timestamp")]
    pub resolution_time: i64,
    /// Traded volume in USDC, as a decimal string (e.g. `"10484.313727"`, `"0.00"`).
    #[serde(rename = "volumeUsdc")]
    pub volume_usdc: Option<String>,
    #[serde(rename = "yesPrice")]
    pub yes_price: Option<String>,
    #[serde(rename = "noPrice")]
    pub no_price: Option<String>,
    /// The bonding-curve (primary-phase) prices. These are populated on rows where the generic
    /// `yesPrice`/`noPrice` are null - on a 50-row sample `primaryYesPrice` was set on all 50
    /// while `yesPrice` was set on only 40 - so a primary-phase market is priced from these
    /// first, falling back to the generic pair.
    #[serde(rename = "primaryYesPrice")]
    pub primary_yes_price: Option<String>,
    #[serde(rename = "primaryNoPrice")]
    pub primary_no_price: Option<String>,
}

/// Terminal states for either `phase` or `status` - a market in any of these can never take
/// another bet again, regardless of which field says so. `secondary`/`secondary_active` are here
/// too: Panta has no secondary-market trading API, so a market that has moved on from `primary`
/// cannot be quoted or routed through this integration even though it isn't finished.
const DEAD_STATES: [&str; 5] = ["resolved", "cancelled", "closed", "secondary", "secondary_active"];

impl From<PantaMarketRow> for VenueMarket {
    fn from(row: PantaMarketRow) -> Self {
        let dead = row.resolved
            || DEAD_STATES.contains(&row.phase.to_lowercase().as_str())
            || DEAD_STATES.contains(&row.status.to_lowercase().as_str());
        let title = if row.title.trim().is_empty() { row.description.trim().to_string() } else { row.title };
        // Prefer the primary-phase prices, which Panta populates far more reliably than the
        // generic pair (50/50 vs 40/50 on a live sample).
        let yes_price = row.primary_yes_price.or(row.yes_price);
        let no_price = row.primary_no_price.or(row.no_price);
        VenueMarket {
            venue_id: "panta".to_string(),
            venue_market_id: row.market_id,
            title,
            // Panta publishes the whole question on the market itself, so there is no parent
            // event to nest and no separate rules field.
            event: None,
            rules: None,
            category: Category::from(row.category),
            phase: row.phase,
            start_time: row.start_time,
            // Panta reports a real betting cutoff; fall back to resolution only when absent.
            close_time: row.end_time.unwrap_or(row.resolution_time),
            resolution_time: row.resolution_time,
            yes_price,
            no_price,
            liquidity: row.volume_usdc.and_then(|v| v.parse().ok()),
            dead,
        }
    }
}

/// Panta's `/markets/` response envelope.
#[derive(Debug, Deserialize)]
pub struct PantaListResponse {
    pub items: Vec<PantaMarketRow>,
    #[serde(rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

/// Panta's `/primaryorderquote/` response.
#[derive(Debug, Deserialize)]
pub struct PantaQuoteResponse {
    pub shares: String,
    #[serde(rename = "avgPrice")]
    pub avg_price: String,
    #[serde(rename = "feeUsdc")]
    pub fee_usdc: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> i64 {
        #[derive(Deserialize)]
        struct Wrapper(#[serde(deserialize_with = "deserialize_timestamp")] i64);
        serde_json::from_str::<Wrapper>(json).unwrap().0
    }

    #[test]
    fn pantas_own_other_bucket_is_reachable() {
        // The bug this guards: "other" fell into the unrecognised catch-all and mapped to None,
        // so creating a market in Panta's `other` category was impossible.
        assert_eq!(category_to_native(&Category::Other), Some("other"));
    }

    #[test]
    fn the_advertised_category_list_matches_what_is_actually_accepted() {
        // The list was hand-written once and drifted, producing an error that rejected "other"
        // while naming "other" as accepted. Deriving it makes that contradiction impossible.
        for name in supported_categories() {
            let parsed = Category::from(name.clone());
            assert!(
                category_to_native(&parsed).is_some(),
                "'{name}' is advertised as accepted but does not map to a Panta category"
            );
        }
    }

    #[test]
    fn a_category_panta_lacks_is_not_advertised() {
        assert!(!supported_categories().contains(&"esports".to_string()));
        assert!(supported_categories().contains(&"other".to_string()));
    }

    #[test]
    fn accepts_a_unix_integer() {
        assert_eq!(parse("1835514000"), 1_835_514_000);
    }

    #[test]
    fn accepts_an_iso8601_string() {
        assert_eq!(parse("\"2026-01-01T00:00:00Z\""), 1_767_225_600);
    }

    #[test]
    fn falls_back_to_zero_on_an_unparseable_string() {
        assert_eq!(parse("\"not-a-timestamp\""), 0);
    }
}

// ── Market creation ─────────────────────────────────────────────────────────────────────────
//
// Panta's create flow is three calls: quote (validate, reserve a session, price it), build (get an
// unsigned transaction), register (tell Panta the signature after broadcast). Field names are
// taken from https://docs.panta.market/api-reference/markets/.
//
// Money here is in **USDC base units** (1_000_000 = $1.00), not decimal USDC - see
// `usdc_base_units_to_decimal`.

use rust_decimal::Decimal;
use serde::Serialize;

/// Converts Panta's USDC base units into decimal USDC.
///
/// Exact integer-scaled `Decimal` division, never through `f64` - a creation fee is real money and
/// float rounding has no business anywhere near it.
pub fn usdc_base_units_to_decimal(raw: &str) -> Option<Decimal> {
    raw.trim().parse::<Decimal>().ok().map(|units| units / Decimal::from(1_000_000))
}

/// `POST /markets/create/quote/` request.
#[derive(Debug, Serialize)]
pub struct PantaCreateQuoteRequest {
    /// Fee payer and signer, base58.
    pub wallet: String,
    /// The question. Max 512 characters.
    pub question: String,
    #[serde(rename = "resolutionRule")]
    pub resolution_rule: String,
    /// Where the answer will be read from. Must be non-empty, max 20.
    #[serde(rename = "sourcesOfTruth")]
    pub sources_of_truth: Vec<String>,
    /// Panta's own vocabulary, not the canonical one - translated by `category_to_native`.
    pub category: String,
    #[serde(rename = "startTime")]
    pub start_time: i64,
    #[serde(rename = "endTime")]
    pub end_time: i64,
    #[serde(rename = "resolutionTime")]
    pub resolution_time: i64,
    #[serde(rename = "imageUrl")]
    pub image_url: String,
    #[serde(rename = "marketType", skip_serializing_if = "Option::is_none")]
    pub market_type: Option<String>,
    /// Defaults to `question` when omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

/// `POST /markets/create/quote/` response.
#[derive(Debug, Deserialize)]
pub struct PantaCreateQuoteResponse {
    /// Session id, carried into build and register.
    #[serde(rename = "createId")]
    pub create_id: String,
    /// Total fee, in USDC base units.
    #[serde(rename = "paymentUsdc")]
    pub payment_usdc: String,
    /// The part of the fee that becomes the new market's own liquidity.
    #[serde(rename = "liquidityInjectionUsdc", default)]
    pub liquidity_injection_usdc: Option<String>,
    /// The part Panta keeps.
    #[serde(rename = "platformRevenueUsdc", default)]
    pub platform_revenue_usdc: Option<String>,
    /// ISO-8601 session expiry - roughly five minutes out.
    ///
    /// Captured but not used as the transaction's deadline: the blockhash from `build` expires in
    /// about sixty seconds, so it binds far tighter and is what a signer actually races. This
    /// matters only if a build is retried against the same session.
    #[allow(dead_code)]
    #[serde(rename = "expiresAt", default)]
    pub expires_at: Option<String>,
}

/// `POST /markets/create/build/` request.
#[derive(Debug, Serialize)]
pub struct PantaCreateBuildRequest {
    #[serde(rename = "createId")]
    pub create_id: String,
    /// Optional, but must match the wallet from quote when sent.
    pub wallet: String,
}

/// `POST /markets/create/build/` response.
#[derive(Debug, Deserialize)]
pub struct PantaCreateBuildResponse {
    /// Base64 unsigned `VersionedTransaction`.
    pub transaction: String,
    /// How long the embedded blockhash stays valid - around 60 seconds, which is the real
    /// constraint on a human signing flow, not the five-minute session.
    #[serde(rename = "blockhashExpiryHintSec", default)]
    pub blockhash_expiry_hint_sec: Option<i64>,
    #[serde(rename = "paymentUsdc", default)]
    pub payment_usdc: Option<String>,
    #[serde(rename = "liquidityInjectionUsdc", default)]
    pub liquidity_injection_usdc: Option<String>,
    #[serde(rename = "platformRevenueUsdc", default)]
    pub platform_revenue_usdc: Option<String>,
}

/// `POST /markets/register/` request.
///
/// Idempotent on this exact pair: repeating it is safe, and once a `createId` has registered one
/// signature it will not accept a different one.
#[derive(Debug, Serialize)]
pub struct PantaRegisterRequest {
    #[serde(rename = "createId")]
    pub create_id: String,
    /// Base58 transaction signature from the broadcast.
    pub signature: String,
}

/// `POST /markets/register/` response.
#[derive(Debug, Deserialize)]
pub struct PantaRegisterResponse {
    /// The event address, which becomes the market id every later API uses.
    #[serde(rename = "marketId", default)]
    pub market_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    /// Echoed back by Panta. Captured so the response shape is complete and visible in Debug,
    /// though the created market's title is read from the catalog rather than from here.
    #[allow(dead_code)]
    #[serde(default)]
    pub title: Option<String>,
}
