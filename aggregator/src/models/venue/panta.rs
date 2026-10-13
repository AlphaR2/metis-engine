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
use super::{Category, VenueEvent, VenueMarket};

/// The categories Panta's market-creation flow accepts - its own `GET /categories/`, confirmed
/// live. Used only to reject a creation request before any call; the catalog refresh reads the
/// live list from the endpoint instead (see `PantaAdapter::filter_categories`).
pub const CREATE_CATEGORIES: [&str; 8] =
    ["sports", "crypto", "politics", "entertainment", "finance", "science", "world", "other"];

/// Panta's `GET /categories/` response.
#[derive(Debug, Deserialize)]
pub struct PantaCategoriesResponse {
    pub categories: Vec<String>,
}

/// One row of Panta's `/markets/` list response.
///
/// `phase` and `status` are two genuinely separate wire fields - `phase` is the trading stage
/// (`primary`/`secondary`/`resolved`/`cancelled`, the same enum our own `status` query param
/// filters on) while `status` is Panta's own tradeability flag (`open`/`resolved`/`cancelled`).
/// Real data confirmed these can disagree (e.g. a market still `phase: "primary"` but already
/// `status: "cancelled"`) - both are captured so `VenueMarket::dead` can catch either signal.
#[derive(Debug, Deserialize, Clone, Default)]
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
    #[serde(default)]
    pub images: Vec<String>,
    /// `standard` or `breaking`.
    #[serde(rename = "marketType", default)]
    pub market_type: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    /// The market's question. **Only on the single-market endpoint** - list rows leave `title`
    /// and `description` blank (0/84 titled in the Phase 0 capture), so `PantaAdapter` fills text
    /// in from the detail call.
    #[serde(default)]
    pub question: Option<String>,
    /// Resolution terms - detail endpoint only.
    #[serde(rename = "resolutionRule", default)]
    pub resolution_rule: Option<String>,
    /// Sources of truth for resolution - detail endpoint only.
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl PantaMarketRow {
    /// The market's question, from whichever field Panta filled.
    pub fn text(&self) -> Option<String> {
        [Some(&self.title), self.question.as_ref(), Some(&self.description)]
            .into_iter()
            .flatten()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
            .map(str::to_string)
    }

    /// Whether this row is in a state it can never leave (resolved or cancelled).
    pub fn is_terminal(&self) -> bool {
        self.resolved
            || ["resolved", "cancelled"].contains(&self.phase.to_lowercase().as_str())
            || ["resolved", "cancelled"].contains(&self.status.to_lowercase().as_str())
    }

    /// Takes what the single-market endpoint knows better than the list row: the text fields
    /// (blank on list rows) and the market's state. List rows lag - in the Phase 0 capture 40 of
    /// 84 disagreed with their detail, e.g. listed `primary`/`open` while actually `cancelled` or
    /// `resolved` - so the detail's state wins.
    pub fn reconcile_with(&mut self, detail: &PantaMarketRow) {
        self.phase = detail.phase.clone();
        self.status = detail.status.clone();
        self.resolved = detail.resolved;
        self.fill_text_from(detail);
    }

    /// Copies the text-bearing fields from the detail endpoint's version of this row.
    fn fill_text_from(&mut self, detail: &PantaMarketRow) {
        if self.title.trim().is_empty() {
            self.title = detail.title.clone();
        }
        if self.description.trim().is_empty() {
            self.description = detail.description.clone();
        }
        self.question = self.question.take().or_else(|| detail.question.clone());
        self.resolution_rule = self.resolution_rule.take().or_else(|| detail.resolution_rule.clone());
        if self.sources.is_empty() {
            self.sources = detail.sources.clone();
        }
    }
}

/// Terminal states for either `phase` or `status` - a market in any of these can never take
/// another bet again, regardless of which field says so.
const DEAD_STATES: [&str; 3] = ["resolved", "cancelled", "closed"];

/// Past the primary phase: still trading on Panta's on-chain order book, which its API does not
/// reach, so shown for its prices but never routed to.
const SECONDARY_STATES: [&str; 2] = ["secondary", "secondary_active"];

impl From<PantaMarketRow> for VenueMarket {
    fn from(row: PantaMarketRow) -> Self {
        let dead = row.resolved
            || DEAD_STATES.contains(&row.phase.to_lowercase().as_str())
            || DEAD_STATES.contains(&row.status.to_lowercase().as_str());
        let display_only = SECONDARY_STATES.contains(&row.phase.to_lowercase().as_str())
            || SECONDARY_STATES.contains(&row.status.to_lowercase().as_str());
        let title = row.text().unwrap_or_default();
        let non_blank = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
        // Panta has no event layer: each market is its own event, so every market has somewhere
        // to carry its description, image and tags.
        let event = (!title.is_empty()).then(|| VenueEvent {
            event_id: row.market_id.clone(),
            title: title.clone(),
            description: non_blank(&row.description).filter(|d| *d != title),
            image_url: row.images.first().cloned(),
            tags: [row.market_type.clone(), row.region.clone()].into_iter().flatten().filter(|t| !t.is_empty()).collect(),
            ..Default::default()
        });
        let rules = match (row.resolution_rule.as_deref().and_then(non_blank), row.sources.is_empty()) {
            (Some(rule), true) => Some(rule),
            (Some(rule), false) => Some(format!("{rule}\n\nSources: {}", row.sources.join(", "))),
            (None, _) => None,
        };
        // Prefer the primary-phase prices, which Panta populates far more reliably than the
        // generic pair (50/50 vs 40/50 on a live sample).
        let yes_price = row.primary_yes_price.or(row.yes_price);
        let no_price = row.primary_no_price.or(row.no_price);
        VenueMarket {
            venue_id: "panta".to_string(),
            venue_market_id: row.market_id,
            title,
            event,
            rules,
            category: Category::from(row.category.as_str()),
            venue_category: row.category,
            phase: row.phase,
            start_time: row.start_time,
            // Panta reports a real betting cutoff; fall back to resolution only when absent.
            close_time: row.end_time.unwrap_or(row.resolution_time),
            resolution_time: row.resolution_time,
            yes_price,
            no_price,
            liquidity: row.volume_usdc.and_then(|v| v.parse().ok()),
            dead,
            display_only,
            extra: row.extra,
            ..Default::default()
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
    /// The handle `/primaryorderbuild/` takes. Absent on old responses; a buy refuses without it.
    #[serde(rename = "quoteId", default)]
    pub quote_id: Option<String>,
    pub shares: String,
    #[serde(rename = "avgPrice")]
    pub avg_price: String,
    #[serde(rename = "feeUsdc")]
    pub fee_usdc: String,
    #[serde(rename = "expiresAt")]
    #[allow(dead_code)]
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
    /// Where the answer is read from, written to the market's oracle metadata. Panta defaults it to the
    /// sources of truth joined by commas, so it is only sent when the caller names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oracle: Option<String>,
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

#[cfg(test)]
mod fixture_tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{}/fixtures/panta/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// The first list row whose detail disagrees with it, from the Phase 0 capture.
    const LISTED_OPEN_ACTUALLY_CANCELLED: &str = "AESrMoZxcTGQibC1rNEmq3oz9qoDqHhomUe6MQEw1k9F";

    fn list_row(id: &str) -> PantaMarketRow {
        let list: PantaListResponse = serde_json::from_str(&fixture("markets-sports")).unwrap();
        list.items.into_iter().find(|r| r.market_id == id).expect("row in fixture")
    }

    #[test]
    fn list_rows_carry_no_text_and_the_detail_supplies_it() {
        let mut row = list_row(LISTED_OPEN_ACTUALLY_CANCELLED);
        assert!(row.text().is_none(), "list rows are blank - that's why details are fetched");
        let detail: PantaMarketRow = serde_json::from_str(&fixture(&format!("market-{LISTED_OPEN_ACTUALLY_CANCELLED}"))).unwrap();
        row.reconcile_with(&detail);
        assert_eq!(row.text().as_deref(), Some("Marco Rubio to win the 2028 Presidential nomination of the Republican Party"));
        let market = VenueMarket::from(row);
        assert!(market.rules.as_deref().unwrap().starts_with("Resolve YES, if Marco Rubio"));
        assert!(market.rules.as_deref().unwrap().contains("Sources: world-politics-reuters"));
    }

    #[test]
    fn the_details_state_overrides_a_stale_list_row() {
        let mut row = list_row(LISTED_OPEN_ACTUALLY_CANCELLED);
        assert!(VenueMarket::from(row.clone()).is_open(0), "listed as open");
        let detail: PantaMarketRow = serde_json::from_str(&fixture(&format!("market-{LISTED_OPEN_ACTUALLY_CANCELLED}"))).unwrap();
        assert!(detail.is_terminal());
        row.reconcile_with(&detail);
        assert!(!VenueMarket::from(row).is_open(0), "cancelled per its own record");
    }

    #[test]
    fn each_market_is_its_own_event_carrying_images_and_tags() {
        let detail: PantaMarketRow = serde_json::from_str(&fixture(&format!("market-{LISTED_OPEN_ACTUALLY_CANCELLED}"))).unwrap();
        let market = VenueMarket::from(detail);
        let event = market.event.expect("an event per market");
        assert_eq!(event.event_id, LISTED_OPEN_ACTUALLY_CANCELLED);
        assert_eq!(event.title, market.title);
        assert_eq!(event.tags, ["standard", "Global"]);
        assert_eq!(market.venue_category, "sports");
        assert!(market.extra.contains_key("oracle"), "unmodelled fields are kept");
    }
}


// ── Orders, claims, positions ───────────────────────────────────────────────────────────────
//
// Panta's buy and claim builds return raw instructions and a blockhash, not a finished
// transaction (its create build does return one). Field names from the published API reference:
// https://github.com/Kaito-HQ/panta-api-pub/tree/main/api-reference. Money here is decimal USDC
// strings (`"20.00"`), unlike the creation fees above, which are base units.

/// One account in an instruction.
#[derive(Debug, Deserialize)]
pub struct PantaInstructionAccount {
    pub pubkey: String,
    #[serde(rename = "isSigner", default)]
    pub is_signer: bool,
    #[serde(rename = "isWritable", default)]
    pub is_writable: bool,
}

/// One instruction to compile into the transaction.
#[derive(Debug, Deserialize)]
pub struct PantaInstruction {
    #[serde(rename = "programId")]
    pub program_id: String,
    /// Base64 instruction data.
    pub data: String,
    #[serde(default)]
    pub accounts: Vec<PantaInstructionAccount>,
}

/// `POST /primaryorderbuild/` response.
#[derive(Debug, Deserialize)]
pub struct PantaOrderBuildResponse {
    #[serde(rename = "orderId")]
    pub order_id: String,
    #[serde(default)]
    pub wallet: Option<String>,
    #[serde(rename = "marketId", default)]
    pub market_id: Option<String>,
    #[serde(rename = "amountUsdc", default)]
    pub amount_usdc: Option<String>,
    #[serde(rename = "expectedShares", default)]
    pub expected_shares: Option<String>,
    #[serde(rename = "feeUsdc", default)]
    pub fee_usdc: Option<String>,
    pub instructions: Vec<PantaInstruction>,
    #[serde(rename = "recentBlockhash")]
    pub recent_blockhash: String,
    #[serde(rename = "lastValidBlockHeight", default)]
    pub last_valid_block_height: Option<u64>,
    #[serde(rename = "expiresAt", default)]
    pub expires_at: Option<String>,
    #[serde(rename = "blockhashExpiryHintSec", default)]
    pub blockhash_expiry_hint_sec: Option<i64>,
}

/// `POST /claim/build/` response.
#[derive(Debug, Deserialize)]
pub struct PantaClaimBuildResponse {
    #[serde(rename = "winningShares", default)]
    pub winning_shares: Option<String>,
    pub instructions: Vec<PantaInstruction>,
    #[serde(rename = "recentBlockhash")]
    pub recent_blockhash: String,
    #[serde(rename = "lastValidBlockHeight", default)]
    pub last_valid_block_height: Option<u64>,
}

/// `POST /primaryordersubmit/` and `/primaryorderverify/` response - the same fields cover both.
#[derive(Debug, Deserialize)]
pub struct PantaOrderStatusResponse {
    /// `built`, `submitted`, `confirmed`, `failed` or `expired`.
    pub status: String,
    #[serde(rename = "marketId", default)]
    pub market_id: Option<String>,
}

/// One row of `GET /positions/`. A wallet holding both sides of a market gets two rows.
#[derive(Debug, Deserialize)]
pub struct PantaPositionRow {
    #[serde(rename = "marketId")]
    pub market_id: String,
    #[serde(default)]
    pub side: String,
    #[serde(default)]
    pub shares: String,
    /// `primary`, `secondary`, `resolved` or `cancelled`.
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub claimable: bool,
    #[serde(default)]
    pub claimed: bool,
    /// `yes` or `no` once resolved, null before.
    #[serde(default)]
    pub outcome: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PantaPositionsResponse {
    #[serde(default)]
    pub positions: Vec<PantaPositionRow>,
}
