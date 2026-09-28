//! Panta market creation: quote, build, register.
//!
//! Three calls, in order:
//!
//! 1. `POST /markets/create/quote/` validates the parameters, reserves a ~5 minute session and
//!    prices the creation fee.
//! 2. `POST /markets/create/build/` returns an unsigned `VersionedTransaction` whose blockhash is
//!    good for roughly 60 seconds.
//! 3. The caller's wallet signs and broadcasts it — **Panta does not broadcast, and neither do we**.
//! 4. `POST /markets/register/` hands Panta the signature so it can verify the on-chain create and
//!    write the catalog metadata.
//!
//! Step 4 is idempotent on `(createId, signature)`, so a caller who loses our response can safely
//! retry rather than being left unsure whether their money bought anything.

use crate::errors::AdapterError;
use crate::models::execution::{
    ActionStatus, ConfirmOutcome, CostLine, CreateMarketIntent, IntentKind, TransactionCost,
    UnsignedTransaction,
};
use crate::models::venue::Category;
use crate::models::venue::panta::{
    PantaCreateBuildRequest, PantaCreateBuildResponse, PantaCreateQuoteRequest,
    CREATE_CATEGORIES, PantaCreateQuoteResponse, PantaRegisterRequest, PantaRegisterResponse,
    usdc_base_units_to_decimal,
};
use crate::utils::http::parse_json_with_diagnostics;
use crate::utils::time::now_unix;

/// Panta requires a market to start at least this far ahead of now, unless it is a `breaking`
/// market with `eventInProgress` set.
const MIN_START_DELAY_SECS: i64 = 3_600;

/// Field limits Panta enforces. Checked here so an over-long question is rejected instantly with a
/// message naming the field, rather than after a round trip.
const MAX_QUESTION: usize = 512;
const MAX_RESOLUTION_RULE: usize = 2_048;
const MAX_IMAGE_URL: usize = 2_048;
const MAX_SOURCES: usize = 20;

/// If Panta gives us no blockhash hint, assume the usual Solana window rather than claiming the
/// transaction never expires — treating an unknown expiry as "no expiry" is how a caller ends up
/// submitting something already dead.
const ASSUMED_BLOCKHASH_SECS: i64 = 60;

/// `venue_ref` prefix for a pending creation.
///
/// Panta's `confirm` has to know whether a reference is a create session or an order before it can
/// pick an endpoint, and a bare id doesn't say. Prefixing makes it self-describing, so no extra
/// lookup or stored state is needed to route a confirmation correctly.
pub const CREATE_REF_PREFIX: &str = "create:";

/// Rejects anything Panta would reject, before spending a call on it.
///
/// Every message names the field and the actual limit, because "invalid parameters" on a form the
/// user just spent minutes filling in is not a usable error.
fn validate(intent: &CreateMarketIntent, now: i64) -> Result<(), AdapterError> {
    let invalid = |msg: String| Err(AdapterError::InvalidRequest(msg));

    if intent.question.trim().is_empty() {
        return invalid("question is required".into());
    }
    if intent.question.chars().count() > MAX_QUESTION {
        return invalid(format!("question is {} characters; the limit is {MAX_QUESTION}", intent.question.chars().count()));
    }
    if intent.resolution_rule.trim().is_empty() {
        return invalid("resolutionRule is required — a market with no stated resolution criteria cannot be settled honestly".into());
    }
    if intent.resolution_rule.chars().count() > MAX_RESOLUTION_RULE {
        return invalid(format!(
            "resolutionRule is {} characters; the limit is {MAX_RESOLUTION_RULE}",
            intent.resolution_rule.chars().count()
        ));
    }
    let sources: Vec<&String> = intent.sources_of_truth.iter().filter(|s| !s.trim().is_empty()).collect();
    if sources.is_empty() {
        return invalid("at least one sourceOfTruth is required".into());
    }
    if sources.len() > MAX_SOURCES {
        return invalid(format!("{} sources of truth; the limit is {MAX_SOURCES}", sources.len()));
    }
    if intent.image_url.trim().is_empty() {
        return invalid("imageUrl is required by Panta".into());
    }
    if intent.image_url.chars().count() > MAX_IMAGE_URL {
        return invalid(format!("imageUrl exceeds {MAX_IMAGE_URL} characters"));
    }
    if !(intent.image_url.starts_with("http://") || intent.image_url.starts_with("https://")) {
        return invalid("imageUrl must be an http(s) URL".into());
    }

    // Times must be strictly ordered, and the start far enough ahead.
    if intent.start_time < now + MIN_START_DELAY_SECS {
        return invalid(format!(
            "startTime must be at least {} minutes ahead of now",
            MIN_START_DELAY_SECS / 60
        ));
    }
    if intent.end_time <= intent.start_time {
        return invalid("endTime must be after startTime".into());
    }
    if intent.resolution_time <= intent.end_time {
        return invalid("resolutionTime must be after endTime".into());
    }

    if native_create_category(&intent.category).is_none() {
        return invalid(format!(
            "Panta has no '{}' category — it accepts {}",
            intent.category,
            CREATE_CATEGORIES.join(", ")
        ));
    }
    Ok(())
}

/// Panta's own spelling of `requested`, if creation accepts it.
fn native_create_category(requested: &str) -> Option<&'static str> {
    let requested = Category::from(requested);
    CREATE_CATEGORIES.into_iter().find(|c| Category::from(*c) == requested)
}

/// Builds the itemized cost from whichever figures Panta returned.
///
/// The total is Panta's own number, never a sum of the parts — if the breakdown doesn't reconcile
/// that is a discrepancy the caller should be able to see, not one we paper over by recomputing.
fn cost_from(
    payment: &str,
    liquidity: Option<&str>,
    platform: Option<&str>,
) -> Result<TransactionCost, AdapterError> {
    let total = usdc_base_units_to_decimal(payment)
        .ok_or_else(|| AdapterError::Parse(format!("panta returned a non-numeric fee: '{payment}'")))?;

    let mut lines = Vec::new();
    if let Some(amount) = liquidity.and_then(usdc_base_units_to_decimal) {
        lines.push(CostLine {
            label: "Liquidity injection".to_string(),
            amount_usdc: amount,
            note: Some("Seeds the new market's own pool — this part stays in the market.".to_string()),
        });
    }
    if let Some(amount) = platform.and_then(usdc_base_units_to_decimal) {
        lines.push(CostLine {
            label: "Platform fee".to_string(),
            amount_usdc: amount,
            note: Some("Panta's cut for hosting and resolving the market.".to_string()),
        });
    }
    Ok(TransactionCost { total_usdc: total, lines })
}

/// Runs quote then build, returning a transaction ready for the caller's wallet.
pub async fn build_create_market(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    intent: &CreateMarketIntent,
) -> Result<UnsignedTransaction, AdapterError> {
    let now = now_unix();
    validate(intent, now)?;

    let native_category =
        native_create_category(&intent.category).expect("validate() already rejected unsupported categories");

    // ── 1. Quote ────────────────────────────────────────────────────────────────────────────
    let quote_body = PantaCreateQuoteRequest {
        wallet: intent.wallet.clone(),
        question: intent.question.trim().to_string(),
        resolution_rule: intent.resolution_rule.trim().to_string(),
        sources_of_truth: intent
            .sources_of_truth
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        category: native_category.to_string(),
        start_time: intent.start_time,
        end_time: intent.end_time,
        resolution_time: intent.resolution_time,
        image_url: intent.image_url.trim().to_string(),
        market_type: None,
        title: intent.title.clone(),
        description: intent.description.clone(),
        region: intent.region.clone(),
    };

    let response = client
        .post(format!("{base_url}/markets/create/quote/"))
        .header("X-Api-Key", api_key)
        .json(&quote_body)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AdapterError::Venue(e.to_string()))?;
    let quote: PantaCreateQuoteResponse =
        parse_json_with_diagnostics(response, "panta create quote").await.map_err(AdapterError::Parse)?;

    // ── 2. Build ────────────────────────────────────────────────────────────────────────────
    let build_body =
        PantaCreateBuildRequest { create_id: quote.create_id.clone(), wallet: intent.wallet.clone() };

    let response = client
        .post(format!("{base_url}/markets/create/build/"))
        .header("X-Api-Key", api_key)
        .json(&build_body)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AdapterError::Venue(e.to_string()))?;
    let built: PantaCreateBuildResponse =
        parse_json_with_diagnostics(response, "panta create build").await.map_err(AdapterError::Parse)?;

    // The blockhash is the binding constraint, not the session: ~60s against ~5 minutes. Expiry is
    // measured from now, since the blockhash was minted by the call we just made.
    let blockhash_window = built.blockhash_expiry_hint_sec.unwrap_or(ASSUMED_BLOCKHASH_SECS);
    let expires_at = now + blockhash_window;

    let cost = cost_from(
        built.payment_usdc.as_deref().unwrap_or(&quote.payment_usdc),
        built
            .liquidity_injection_usdc
            .as_deref()
            .or(quote.liquidity_injection_usdc.as_deref()),
        built.platform_revenue_usdc.as_deref().or(quote.platform_revenue_usdc.as_deref()),
    )?;

    Ok(UnsignedTransaction {
        venue_id: "panta".to_string(),
        kind: IntentKind::CreateMarket,
        transaction: built.transaction,
        venue_ref: format!("{CREATE_REF_PREFIX}{}", quote.create_id),
        expires_at,
        required_signers: vec![intent.wallet.clone()],
        // Panta returns `lastValidBlockHeight` on build, but its own `blockhashExpiryHintSec` is
        // already a wall-clock figure, so the estimate above is sound without it.
        last_valid_block_height: None,
        cost,
        // Creating a market yields a market, not a position - there is nothing to preview.
        preview: None,
    })
}

/// Tells Panta a created market's transaction was broadcast.
pub async fn register(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    create_id: &str,
    signature: &str,
) -> Result<ConfirmOutcome, AdapterError> {
    let body = PantaRegisterRequest {
        create_id: create_id.to_string(),
        signature: signature.to_string(),
    };

    let response = client
        .post(format!("{base_url}/markets/register/"))
        .header("X-Api-Key", api_key)
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    let text = response.text().await.map_err(AdapterError::Http)?;

    if !status.is_success() {
        // Panta fails closed with named codes - CREATE_EXPIRED, TX_NOT_FOUND, TX_FAILED,
        // TX_MISMATCH, TX_FEE_MISMATCH, INVALID_MARKET_PARAMS, RATE_LIMITED. Each says something
        // different and actionable, so the body is passed through rather than flattened.
        //
        // RATE_LIMITED says nothing about the transaction itself: it is still in flight, so it
        // stays pending and the caller retries, rather than being told it failed.
        let status = if text.contains("CREATE_EXPIRED") {
            ActionStatus::Expired
        } else if text.contains("RATE_LIMITED") || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            ActionStatus::Submitted
        } else {
            ActionStatus::Failed
        };
        return Ok(ConfirmOutcome {
            venue_ref: create_id.to_string(),
            status,
            venue_market_id: None,
            message: Some(text),
        });
    }

    let registered: PantaRegisterResponse =
        serde_json::from_str(&text).map_err(|e| AdapterError::Parse(format!("panta register: {e} - body: {text}")))?;

    Ok(ConfirmOutcome {
        venue_ref: create_id.to_string(),
        status: ActionStatus::Confirmed,
        venue_market_id: registered.market_id,
        message: registered.status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    const NOW: i64 = 1_000_000;
    const HOUR: i64 = 3_600;

    fn intent() -> CreateMarketIntent {
        CreateMarketIntent {
            venue_id: "panta".to_string(),
            question: "Will it rain in Lagos on Friday?".to_string(),
            resolution_rule: "Resolves YES if NIMET reports measurable rainfall.".to_string(),
            sources_of_truth: vec!["https://nimet.gov.ng".to_string()],
            category: "science".to_string(),
            start_time: NOW + 2 * HOUR,
            end_time: NOW + 20 * HOUR,
            resolution_time: NOW + 24 * HOUR,
            image_url: "https://example.com/img.png".to_string(),
            wallet: "Wallet111".to_string(),
            title: None,
            description: None,
            region: None,
        }
    }

    #[test]
    fn a_well_formed_intent_validates() {
        assert!(validate(&intent(), NOW).is_ok());
    }

    #[test]
    fn a_start_time_inside_the_minimum_delay_is_rejected() {
        let mut i = intent();
        i.start_time = NOW + 60;
        let err = validate(&i, NOW).unwrap_err().to_string();
        assert!(err.contains("60 minutes"), "should name the actual limit, got: {err}");
    }

    #[test]
    fn times_must_be_strictly_ordered() {
        let mut i = intent();
        i.end_time = i.start_time;
        assert!(validate(&i, NOW).is_err());

        let mut i = intent();
        i.resolution_time = i.end_time;
        assert!(validate(&i, NOW).is_err());
    }

    #[test]
    fn a_market_with_no_source_of_truth_is_rejected() {
        let mut i = intent();
        i.sources_of_truth = vec!["   ".to_string()];
        assert!(validate(&i, NOW).is_err());
    }

    #[test]
    fn an_over_long_question_is_rejected_naming_the_limit() {
        let mut i = intent();
        i.question = "x".repeat(MAX_QUESTION + 1);
        let err = validate(&i, NOW).unwrap_err().to_string();
        assert!(err.contains("512"), "got: {err}");
    }

    #[test]
    fn a_category_panta_does_not_have_is_rejected_before_any_call() {
        let mut i = intent();
        i.category = "esports".to_string(); // Jupiter has it, Panta does not
        let err = validate(&i, NOW).unwrap_err().to_string();
        assert!(err.contains("esports"), "got: {err}");
    }

    #[test]
    fn a_non_http_image_url_is_rejected() {
        let mut i = intent();
        i.image_url = "ipfs://whatever".to_string();
        assert!(validate(&i, NOW).is_err());
    }

    #[test]
    fn base_units_convert_to_decimal_usdc() {
        assert_eq!(usdc_base_units_to_decimal("25000000"), Some(Decimal::new(25, 0)));
        assert_eq!(usdc_base_units_to_decimal("1500000"), Some(Decimal::new(15, 1)));
        assert_eq!(usdc_base_units_to_decimal("not-a-number"), None);
    }

    #[test]
    fn the_cost_breakdown_keeps_pantas_total_rather_than_summing_the_parts() {
        // A discrepancy between the parts and the total is something to surface, not silently fix.
        let cost = cost_from("25000000", Some("20000000"), Some("4000000")).unwrap();
        assert_eq!(cost.total_usdc, Decimal::new(25, 0));
        assert_eq!(cost.lines.len(), 2);
        assert!(!cost.lines_reconcile(), "20 + 4 != 25, and that must remain visible");
    }

    #[test]
    fn a_reconciling_breakdown_is_reported_as_such() {
        let cost = cost_from("25000000", Some("20000000"), Some("5000000")).unwrap();
        assert!(cost.lines_reconcile());
    }
}
