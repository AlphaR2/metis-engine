//! Panta primary buys, claims and positions.
//!
//! **Buy** is four calls: `POST /primaryorderquote/` prices it and opens a ~90 second session,
//! `POST /primaryorderbuild/` returns instructions, the caller's wallet signs and broadcasts,
//! `POST /primaryordersubmit/` registers the signature, and `POST /primaryorderverify/` reads the
//! order back. Only the **primary** phase can be bought through the API; a market that has moved to
//! secondary trading is refused by Panta (`MARKET_NOT_IN_PRIMARY`).
//!
//! **Claim** is `POST /claim/build/`, which returns instructions and fails closed on-chain if
//! nothing is claimable.
//!
//! Both builds return raw instructions and a blockhash, so they are compiled here into one
//! unsigned versioned transaction (see `utils::solana_tx`), with every program checked against an
//! allowlist first. Panta does not broadcast and neither do we.
//!
//! Two different truths, kept apart: **the chain** says whether the money moved, **Panta** says
//! whether it registered the order. A registration failure (`submit`) never means the transaction
//! did not land, so only a venue that explicitly answered is reported as failed, and a transport
//! error is left as an error the client retries (submit is idempotent on `orderId` + signature).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::execution::{
    ActionStatus, BuyIntent, ClaimIntent, ConfirmOutcome, CostLine, IntentKind, OrderPreview, TransactionCost,
    UnsignedTransaction, claim_ref, parse_claim_ref,
};
use crate::models::position::{Position, PositionState};
use crate::models::venue::panta::{
    PantaClaimBuildResponse, PantaInstruction, PantaOrderBuildResponse, PantaOrderStatusResponse,
    PantaPositionRow, PantaPositionsResponse, PantaQuoteResponse,
};
use crate::utils::http::{check_status, parse_json_with_diagnostics};
use crate::utils::solana_tx::{self, AccountMeta, CompileError, Instruction, decode_32};
use crate::utils::time::now_unix;

/// `venue_ref` prefix for a pending buy, so `confirm` and `status` route to the order endpoints
/// without stored state (see `create::CREATE_REF_PREFIX`).
pub const ORDER_REF_PREFIX: &str = "order:";

/// Slippage allowed when the caller set no floor. Panta's own default.
const DEFAULT_SLIPPAGE_BPS: u32 = 100;
/// Panta's ceiling on `maxSlippageBps`.
const MAX_SLIPPAGE_BPS: u32 = 5_000;
/// Used when Panta gives no blockhash hint: the usual Solana window, never "no expiry".
const ASSUMED_BLOCKHASH_SECS: i64 = 60;
/// Panta caps a positions response at about this many rows. A response that hits it may be cut
/// short, so it is reported as a failure rather than returned as if complete.
const POSITION_ROW_CAP: usize = 200;

fn invalid_wallet() -> AdapterError {
    AdapterError::InvalidRequest("wallet is not a valid Solana address".to_string())
}

/// Compiles Panta's instructions into base64 unsigned-transaction bytes.
///
/// Refused, not repaired, when anything is off: a program outside the allowlist, a signer other
/// than the wallet, an oversized result. Those are a venue drifting or a response being tampered
/// with, and the safe answer to both is to build nothing.
fn compile(
    wallet: &str,
    instructions: &[PantaInstruction],
    recent_blockhash: &str,
    venue_program: &str,
) -> Result<String, AdapterError> {
    let payer = decode_32(wallet).map_err(|_| invalid_wallet())?;
    let blockhash = decode_32(recent_blockhash).map_err(|e| AdapterError::Parse(format!("panta blockhash: {e}")))?;

    let compiled: Vec<Instruction> = instructions
        .iter()
        .map(|ix| {
            let data = BASE64
                .decode(ix.data.trim())
                .map_err(|_| AdapterError::Parse("panta instruction data is not base64".to_string()))?;
            let accounts = ix
                .accounts
                .iter()
                .map(|a| {
                    Ok(AccountMeta {
                        pubkey: decode_32(&a.pubkey).map_err(|e| AdapterError::Parse(format!("panta account: {e}")))?,
                        is_signer: a.is_signer,
                        is_writable: a.is_writable,
                    })
                })
                .collect::<Result<Vec<_>, AdapterError>>()?;
            Ok(Instruction {
                program_id: decode_32(&ix.program_id).map_err(|e| AdapterError::Parse(format!("panta program id: {e}")))?,
                accounts,
                data,
            })
        })
        .collect::<Result<_, AdapterError>>()?;

    let mut allowed = vec![venue_program];
    allowed.extend(solana_tx::COMMON_PROGRAMS);
    let to_error = |e: CompileError| match e {
        CompileError::BadKey(_) => AdapterError::Parse(format!("panta: {e}")),
        _ => AdapterError::Venue(format!("panta: {e}")),
    };
    solana_tx::check_programs(&compiled, &allowed).map_err(to_error)?;
    let bytes = solana_tx::compile_unsigned_v0(payer, &compiled, blockhash).map_err(to_error)?;
    Ok(BASE64.encode(bytes))
}

/// How much slippage to allow, in basis points, from the caller's `min_shares_out` and the quote.
///
/// With no floor Panta's default applies. With one, the tolerance is how far below the quote the
/// floor sits, so the venue itself rejects the build if the curve has moved further than the caller
/// accepted. Capped at Panta's ceiling. A floor at or above the quote gives zero: any movement
/// against the caller is refused.
fn slippage_bps(min_shares_out: Option<Decimal>, quoted_shares: Decimal) -> u32 {
    let Some(floor) = min_shares_out else {
        return DEFAULT_SLIPPAGE_BPS;
    };
    if quoted_shares <= Decimal::ZERO || floor >= quoted_shares {
        return 0;
    }
    let bps = ((quoted_shares - floor) / quoted_shares * Decimal::from(10_000)).floor();
    u32::try_from(bps).unwrap_or(MAX_SLIPPAGE_BPS).min(MAX_SLIPPAGE_BPS)
}

/// When the built transaction stops being worth signing: the blockhash window, or the order's own
/// expiry if that comes sooner.
fn expiry(now: i64, hint_secs: Option<i64>, expires_at: Option<&str>) -> i64 {
    let window = hint_secs.filter(|s| *s > 0).unwrap_or(ASSUMED_BLOCKHASH_SECS).min(600);
    let by_blockhash = now + window;
    match expires_at.and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok()) {
        Some(order_expiry) => by_blockhash.min(order_expiry.timestamp()),
        None => by_blockhash,
    }
}

fn parse_decimal(field: &str, raw: &str) -> Result<Decimal, AdapterError> {
    raw.trim().parse().map_err(|_| AdapterError::Parse(format!("panta {field} = '{raw}' is not a decimal")))
}

/// Asks Panta for a priced, short-lived quote for **this wallet** (a quote is bound to the wallet
/// that will build from it).
async fn request_quote(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    intent: &BuyIntent,
    side: &str,
) -> Result<PantaQuoteResponse, AdapterError> {
    let response = client
        .post(format!("{base_url}/primaryorderquote/"))
        .header("X-Api-Key", api_key)
        .json(&serde_json::json!({
            "wallet": intent.wallet,
            "marketId": intent.venue_market_id,
            "side": side,
            "amountUsdc": intent.amount_usdc.to_string(),
        }))
        .send()
        .await?;
    let response = check_status(response, "panta order quote").await?;
    parse_json_with_diagnostics(response, "panta order quote").await.map_err(AdapterError::Parse)
}

/// Builds a primary buy. Does not sign and does not broadcast.
pub async fn build_buy(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    venue_program: &str,
    intent: &BuyIntent,
) -> Result<UnsignedTransaction, AdapterError> {
    let side = match intent.side.trim().to_lowercase().as_str() {
        "yes" => "yes",
        "no" => "no",
        other => return Err(AdapterError::InvalidRequest(format!("unknown side '{other}' - Panta markets are yes/no"))),
    };
    if intent.amount_usdc <= Decimal::ZERO {
        return Err(AdapterError::InvalidRequest("amount_usdc must be greater than zero".to_string()));
    }
    decode_32(&intent.wallet).map_err(|_| invalid_wallet())?;

    let quote = request_quote(client, base_url, api_key, intent, side).await?;
    let quote_id = quote
        .quote_id
        .clone()
        .ok_or_else(|| AdapterError::Parse("panta order quote carried no quoteId".to_string()))?;
    let quoted_shares = parse_decimal("quote shares", &quote.shares)?;
    let max_slippage = slippage_bps(intent.min_shares_out, quoted_shares);

    let response = client
        .post(format!("{base_url}/primaryorderbuild/"))
        .header("X-Api-Key", api_key)
        .json(&serde_json::json!({
            "quoteId": quote_id,
            "wallet": intent.wallet,
            "maxSlippageBps": max_slippage,
        }))
        .send()
        .await?;
    let response = check_status(response, "panta order build").await?;
    let built: PantaOrderBuildResponse =
        parse_json_with_diagnostics(response, "panta order build").await.map_err(AdapterError::Parse)?;

    // What was built must be what was asked for. A build for another wallet or market is refused
    // rather than handed to the user's wallet to sign.
    if built.wallet.as_deref().is_some_and(|w| w != intent.wallet) {
        return Err(AdapterError::Venue("panta built the order for a different wallet than requested".to_string()));
    }
    if built.market_id.as_deref().is_some_and(|m| m != intent.venue_market_id) {
        return Err(AdapterError::Venue("panta built the order for a different market than requested".to_string()));
    }

    let now = now_unix();
    let transaction = compile(&intent.wallet, &built.instructions, &built.recent_blockhash, venue_program)?;

    let amount = built.amount_usdc.as_deref().map(|a| parse_decimal("amountUsdc", a)).transpose()?.unwrap_or(intent.amount_usdc);
    let fee = built.fee_usdc.as_deref().or(Some(&quote.fee_usdc)).and_then(|f| f.trim().parse::<Decimal>().ok());
    let shares = built.expected_shares.as_deref().map(|s| parse_decimal("expectedShares", s)).transpose()?.unwrap_or(quoted_shares);

    let mut lines = Vec::new();
    if let Some(fee) = fee.filter(|f| *f > Decimal::ZERO) {
        lines.push(CostLine {
            label: "Panta fee".to_string(),
            amount_usdc: fee,
            note: Some("Reported by Panta with the quote. It is part of the amount you spend, not added on top.".to_string()),
        });
    }

    Ok(UnsignedTransaction {
        venue_id: "panta".to_string(),
        kind: IntentKind::BuyShares,
        transaction,
        venue_ref: format!("{ORDER_REF_PREFIX}{}", built.order_id),
        expires_at: expiry(now, built.blockhash_expiry_hint_sec, built.expires_at.as_deref()),
        required_signers: vec![intent.wallet.clone()],
        last_valid_block_height: built.last_valid_block_height,
        cost: TransactionCost { total_usdc: amount, lines },
        preview: Some(OrderPreview {
            contracts: Some(shares.to_string()),
            avg_price_usdc: quote.avg_price.trim().parse().ok(),
            // A winning share pays 1 USDC.
            payout_if_win_usdc: Some(shares),
            slippage_bps: None,
            // Unlike Jupiter's, this ceiling is the caller's own: derived from their floor.
            max_slippage_bps: Some(i64::from(max_slippage)),
        }),
    })
}

/// Registers a broadcast buy with Panta and reports the order's state.
///
/// Idempotent on `(orderId, signature)`, so retrying after a lost response is safe. Only a clear
/// answer from Panta becomes a status; a transport failure or a 5xx is returned as an error so the
/// caller retries, because it says nothing about whether the transaction landed.
pub async fn submit_order(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    order_id: &str,
    signature: &str,
) -> Result<ConfirmOutcome, AdapterError> {
    let response = client
        .post(format!("{base_url}/primaryordersubmit/"))
        .header("X-Api-Key", api_key)
        .json(&serde_json::json!({ "orderId": order_id, "signature": signature }))
        .send()
        .await?;

    let status = response.status();
    if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(AdapterError::Venue(format!("panta order submit: HTTP {}", status.as_u16())));
    }
    if !status.is_success() {
        let text = response.text().await.map_err(AdapterError::Http)?;
        let expired = text.contains("EXPIRED");
        return Ok(ConfirmOutcome {
            venue_ref: format!("{ORDER_REF_PREFIX}{order_id}"),
            status: if expired { ActionStatus::Expired } else { ActionStatus::Failed },
            venue_market_id: None,
            // Registration, not the transaction: the chain is the authority on whether it landed.
            message: Some(crate::errors::adapter_error::redact_urls(&text).chars().take(300).collect()),
        });
    }
    let body: PantaOrderStatusResponse =
        parse_json_with_diagnostics(response, "panta order submit").await.map_err(AdapterError::Parse)?;
    Ok(ConfirmOutcome {
        venue_ref: format!("{ORDER_REF_PREFIX}{order_id}"),
        status: map_order_status(&body.status),
        venue_market_id: body.market_id,
        message: Some(body.status),
    })
}

/// Reads an order's state back. Sends no signature, so it changes nothing and is safe to poll.
pub async fn order_status(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    order_id: &str,
) -> Result<ConfirmOutcome, AdapterError> {
    let response = client
        .post(format!("{base_url}/primaryorderverify/"))
        .header("X-Api-Key", api_key)
        .json(&serde_json::json!({ "orderId": order_id }))
        .send()
        .await?;
    let response = check_status(response, "panta order verify").await?;
    let body: PantaOrderStatusResponse =
        parse_json_with_diagnostics(response, "panta order verify").await.map_err(AdapterError::Parse)?;
    Ok(ConfirmOutcome {
        venue_ref: format!("{ORDER_REF_PREFIX}{order_id}"),
        status: map_order_status(&body.status),
        venue_market_id: body.market_id,
        message: Some(body.status),
    })
}

/// Panta's order status onto ours. An unrecognised value is not terminal: treating an unknown word
/// as a failure would tell a user their money vanished over a vocabulary change.
fn map_order_status(raw: &str) -> ActionStatus {
    match raw.trim().to_lowercase().as_str() {
        "built" => ActionStatus::Built,
        "confirmed" => ActionStatus::Confirmed,
        "failed" => ActionStatus::Failed,
        "expired" => ActionStatus::Expired,
        _ => ActionStatus::Submitted,
    }
}

/// Builds a claim for a resolved, winning market. Does not sign and does not broadcast.
pub async fn build_claim(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    venue_program: &str,
    intent: &ClaimIntent,
) -> Result<UnsignedTransaction, AdapterError> {
    if intent.kind != IntentKind::ClaimWinnings {
        return Err(AdapterError::Unsupported { venue: "panta", action: "creator-fee claims" });
    }
    let (owner, market_id) = parse_claim_ref(&intent.venue_ref).ok_or_else(|| {
        AdapterError::InvalidRequest("venue_ref is not a panta claim reference - use claim_ref from /v1/positions".to_string())
    })?;
    if owner != intent.wallet {
        return Err(AdapterError::InvalidRequest("this claim reference belongs to a different wallet".to_string()));
    }
    decode_32(&intent.wallet).map_err(|_| invalid_wallet())?;

    let response = client
        .post(format!("{base_url}/claim/build/"))
        .header("X-Api-Key", api_key)
        .json(&serde_json::json!({ "wallet": intent.wallet, "marketId": market_id }))
        .send()
        .await?;
    let response = check_status(response, "panta claim build").await?;
    let built: PantaClaimBuildResponse =
        parse_json_with_diagnostics(response, "panta claim build").await.map_err(AdapterError::Parse)?;

    let transaction = compile(&intent.wallet, &built.instructions, &built.recent_blockhash, venue_program)?;
    let shares = built.winning_shares.as_deref().and_then(|s| s.trim().parse::<Decimal>().ok());
    Ok(UnsignedTransaction {
        venue_id: "panta".to_string(),
        kind: IntentKind::ClaimWinnings,
        transaction,
        venue_ref: claim_ref(owner, market_id),
        expires_at: now_unix() + ASSUMED_BLOCKHASH_SECS,
        required_signers: vec![intent.wallet.clone()],
        last_valid_block_height: built.last_valid_block_height,
        // A claim only pays in.
        cost: TransactionCost { total_usdc: Decimal::ZERO, lines: vec![] },
        preview: Some(OrderPreview {
            contracts: shares.map(|s| s.to_string()),
            // A winning share pays 1 USDC.
            payout_if_win_usdc: shares,
            ..Default::default()
        }),
    })
}

/// Where a Panta position stands.
fn state_of(row: &PantaPositionRow) -> PositionState {
    if row.phase.eq_ignore_ascii_case("cancelled") {
        PositionState::Cancelled
    } else if row.claimed {
        PositionState::Claimed
    } else if row.claimable {
        PositionState::Claimable
    } else if row.phase.eq_ignore_ascii_case("resolved")
        && row.outcome.as_deref().is_some_and(|outcome| !outcome.eq_ignore_ascii_case(&row.side))
    {
        PositionState::Lost
    } else {
        // Includes a resolved winner whose claim is not yet enabled: still waiting, not lost.
        PositionState::Open
    }
}

/// Maps one Panta row into the venue-neutral shape. Panta reports shares and a phase only, so cost
/// and value stay unknown here rather than being invented; the API layer prices open rows from the
/// catalog.
pub fn to_position(row: PantaPositionRow, wallet: &str) -> Position {
    let state = state_of(&row);
    let contracts = row.shares.trim().parse::<Decimal>().unwrap_or_default();
    let payout = (state == PositionState::Claimable).then_some(contracts);
    Position {
        venue_id: "panta".to_string(),
        venue_position_id: None,
        claim_ref: (state == PositionState::Claimable).then(|| claim_ref(wallet, &row.market_id)),
        // Panta's API trades the primary phase only and has no sell.
        can_sell: false,
        title: None,
        venue_market_id: row.market_id,
        side: row.side.to_lowercase(),
        contracts,
        avg_price_usdc: None,
        cost_usdc: None,
        value_usdc: None,
        pnl_usdc: None,
        value_is_estimate: false,
        payout_usdc: payout,
        state,
        phase: (!row.phase.is_empty()).then_some(row.phase),
        open_orders: 0,
        event_id: None,
        event_title: None,
        updated_at: None,
    }
}

async fn fetch_rows(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    wallet: &str,
) -> Result<Vec<PantaPositionRow>, AdapterError> {
    decode_32(wallet).map_err(|_| invalid_wallet())?;
    let response = client
        .get(format!("{base_url}/positions/"))
        .header("X-Api-Key", api_key)
        .query(&[("wallet", wallet)])
        .send()
        .await?;
    let response = check_status(response, "panta positions").await?;
    let body: PantaPositionsResponse =
        parse_json_with_diagnostics(response, "panta positions").await.map_err(AdapterError::Parse)?;
    if body.positions.len() >= POSITION_ROW_CAP {
        return Err(AdapterError::Venue(format!(
            "panta returned {} rows, its cap, so this wallet's list may be incomplete",
            body.positions.len()
        )));
    }
    Ok(body.positions)
}

/// Every position the wallet holds on Panta. Zero-share rows are dropped.
pub async fn list_positions(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    wallet: &str,
) -> Result<Vec<Position>, AdapterError> {
    Ok(fetch_rows(client, base_url, api_key, wallet)
        .await?
        .into_iter()
        .map(|row| to_position(row, wallet))
        .filter(|p| p.contracts > Decimal::ZERO)
        .collect())
}

/// A claim's status, read back from the wallet's positions. Read-only.
///
/// It can confirm a claim; it cannot see one fail on-chain, so an unclaimed market is reported as
/// still waiting and the client's own signature check against the chain decides failure.
pub async fn claim_status(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    venue_ref: &str,
) -> Result<ConfirmOutcome, AdapterError> {
    let (wallet, market_id) = parse_claim_ref(venue_ref)
        .ok_or_else(|| AdapterError::InvalidRequest(format!("'{venue_ref}' is not a panta claim reference")))?;
    let rows = fetch_rows(client, base_url, api_key, wallet).await?;
    let ours: Vec<&PantaPositionRow> = rows.iter().filter(|r| r.market_id == market_id).collect();
    let (status, message) = if ours.iter().any(|r| r.claimed) {
        (ActionStatus::Confirmed, "Claimed.")
    } else if ours.is_empty() {
        (ActionStatus::Submitted, "Panta has no record of this position yet.")
    } else {
        (ActionStatus::Submitted, "The claim has not been recorded yet.")
    };
    Ok(ConfirmOutcome {
        venue_ref: venue_ref.to_string(),
        status,
        venue_market_id: Some(market_id.to_string()),
        message: Some(message.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::venue::panta::PantaInstructionAccount;

    fn dec(text: &str) -> Decimal {
        text.parse().unwrap()
    }

    // Real, valid 32-byte keys: the system and token programs.
    const WALLET: &str = solana_tx::TOKEN_PROGRAM;
    const ZERO_HASH: &str = solana_tx::SYSTEM_PROGRAM;
    const VENUE: &str = solana_tx::ASSOCIATED_TOKEN_PROGRAM;

    fn ix(program: &str, accounts: &[(&str, bool, bool)], data: &[u8]) -> PantaInstruction {
        PantaInstruction {
            program_id: program.to_string(),
            data: BASE64.encode(data),
            accounts: accounts
                .iter()
                .map(|(k, s, w)| PantaInstructionAccount { pubkey: k.to_string(), is_signer: *s, is_writable: *w })
                .collect(),
        }
    }

    // ── slippage ────────────────────────────────────────────────────────────────────────────

    #[test]
    fn no_floor_uses_the_venue_default() {
        assert_eq!(slippage_bps(None, dec("38.4")), DEFAULT_SLIPPAGE_BPS);
    }

    #[test]
    fn a_floor_becomes_the_tolerance_below_the_quote() {
        // 1% below the quote.
        assert_eq!(slippage_bps(Some(dec("38.016")), dec("38.4")), 100);
        // 5% below.
        assert_eq!(slippage_bps(Some(dec("36.48")), dec("38.4")), 500);
    }

    #[test]
    fn a_floor_at_or_above_the_quote_allows_no_movement() {
        assert_eq!(slippage_bps(Some(dec("38.4")), dec("38.4")), 0);
        assert_eq!(slippage_bps(Some(dec("40")), dec("38.4")), 0);
    }

    #[test]
    fn tolerance_never_exceeds_the_venue_ceiling_or_goes_negative() {
        assert_eq!(slippage_bps(Some(dec("1")), dec("38.4")), MAX_SLIPPAGE_BPS);
        assert_eq!(slippage_bps(Some(Decimal::ZERO), dec("38.4")), MAX_SLIPPAGE_BPS);
        assert_eq!(slippage_bps(Some(dec("5")), Decimal::ZERO), 0, "an unusable quote allows nothing");
    }

    #[test]
    fn slippage_is_a_whole_number_of_basis_points_rounded_toward_the_caller() {
        // 0.5% and a bit: floors, so the venue is never looser than the caller asked.
        let bps = slippage_bps(Some(dec("38.19")), dec("38.4"));
        assert_eq!(bps, 54, "0.546875% is 54 bps, not 55");
    }

    // ── expiry ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn expiry_is_the_blockhash_window_unless_the_order_expires_sooner() {
        assert_eq!(expiry(1_000, Some(60), None), 1_060);
        assert_eq!(expiry(1_000, None, None), 1_000 + ASSUMED_BLOCKHASH_SECS);
        // 2026-01-01T00:00:00Z is far in the past of now=2_000_000_000, so it wins.
        assert_eq!(expiry(2_000_000_000, Some(60), Some("2026-01-01T00:00:00Z")), 1_767_225_600);
        // An order expiry later than the window changes nothing.
        assert_eq!(expiry(1_000, Some(60), Some("2026-01-01T00:00:00Z")), 1_060);
    }

    #[test]
    fn an_absurd_or_missing_hint_never_means_no_expiry() {
        assert_eq!(expiry(1_000, Some(0), None), 1_060);
        assert_eq!(expiry(1_000, Some(-5), None), 1_060);
        assert_eq!(expiry(1_000, Some(86_400), None), 1_600, "capped at ten minutes");
        assert_eq!(expiry(1_000, Some(60), Some("not a date")), 1_060);
    }

    // ── status ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn order_status_maps_and_an_unknown_word_is_not_a_failure() {
        assert_eq!(map_order_status("built"), ActionStatus::Built);
        assert_eq!(map_order_status("Submitted"), ActionStatus::Submitted);
        assert_eq!(map_order_status("confirmed"), ActionStatus::Confirmed);
        assert_eq!(map_order_status("failed"), ActionStatus::Failed);
        assert_eq!(map_order_status("expired"), ActionStatus::Expired);
        assert_eq!(map_order_status("some_new_state"), ActionStatus::Submitted);
    }

    // ── compiling ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn instructions_compile_into_an_unsigned_transaction_for_the_wallet() {
        let instructions = [ix(VENUE, &[(WALLET, true, true)], &[7, 7])];
        let encoded = compile(WALLET, &instructions, ZERO_HASH, VENUE).unwrap();
        let bytes = BASE64.decode(encoded).unwrap();
        assert_eq!(bytes[0], 1, "one signature slot, for the wallet");
        assert!(bytes[1..65].iter().all(|b| *b == 0), "nothing is signed here");
        assert_eq!(bytes[65], 0x80, "a v0 message");
    }

    #[test]
    fn a_program_outside_the_allowlist_is_refused() {
        let stranger = "Stake11111111111111111111111111111111111111";
        let err = compile(WALLET, &[ix(stranger, &[(WALLET, true, true)], &[1])], ZERO_HASH, VENUE).unwrap_err();
        assert!(matches!(err, AdapterError::Venue(m) if m.contains("unexpected program")));
    }

    #[test]
    fn the_configured_venue_program_is_allowed_and_a_stranger_is_not() {
        // The vote program is not on the common list, so only naming it as the venue's own
        // program lets it through.
        let custom = "Vote111111111111111111111111111111111111111";
        let instructions = [ix(custom, &[(WALLET, true, true)], &[1])];
        assert!(compile(WALLET, &instructions, ZERO_HASH, VENUE).is_err());
        assert!(compile(WALLET, &instructions, ZERO_HASH, custom).is_ok());
    }

    #[test]
    fn a_second_signer_is_refused() {
        let other = solana_tx::MEMO_PROGRAM;
        let instructions = [ix(VENUE, &[(WALLET, true, true), (other, true, false)], &[1])];
        let err = compile(WALLET, &instructions, ZERO_HASH, VENUE).unwrap_err();
        assert!(matches!(err, AdapterError::Venue(m) if m.contains("signer other than your wallet")));
    }

    #[test]
    fn a_bad_wallet_blockhash_or_payload_is_refused_not_repaired() {
        let good = [ix(VENUE, &[(WALLET, true, true)], &[1])];
        assert!(matches!(compile("not-a-key", &good, ZERO_HASH, VENUE), Err(AdapterError::InvalidRequest(_))));
        assert!(matches!(compile(WALLET, &good, "short", VENUE), Err(AdapterError::Parse(_))));
        let mut bad_data = ix(VENUE, &[(WALLET, true, true)], &[1]);
        bad_data.data = "!!!".to_string();
        assert!(matches!(compile(WALLET, &[bad_data], ZERO_HASH, VENUE), Err(AdapterError::Parse(_))));
        assert!(compile(WALLET, &[], ZERO_HASH, VENUE).is_err(), "no instructions is not a transaction");
    }

    // ── positions ───────────────────────────────────────────────────────────────────────────

    fn row(side: &str, shares: &str, phase: &str, claimable: bool, claimed: bool, outcome: Option<&str>) -> PantaPositionRow {
        PantaPositionRow {
            market_id: "MKT".to_string(),
            side: side.to_string(),
            shares: shares.to_string(),
            phase: phase.to_string(),
            claimable,
            claimed,
            outcome: outcome.map(str::to_string),
        }
    }

    #[test]
    fn positions_map_to_the_right_state() {
        let state = |r| to_position(r, "W").state;
        assert_eq!(state(row("yes", "5", "primary", false, false, None)), PositionState::Open);
        assert_eq!(state(row("yes", "5", "secondary", false, false, None)), PositionState::Open);
        assert_eq!(state(row("yes", "5", "resolved", true, false, Some("yes"))), PositionState::Claimable);
        assert_eq!(state(row("yes", "5", "resolved", false, true, Some("yes"))), PositionState::Claimed);
        assert_eq!(state(row("no", "5", "resolved", false, false, Some("yes"))), PositionState::Lost);
        assert_eq!(state(row("yes", "5", "cancelled", false, false, None)), PositionState::Cancelled);
    }

    #[test]
    fn a_resolved_winner_not_yet_claimable_is_waiting_not_lost() {
        assert_eq!(to_position(row("yes", "5", "resolved", false, false, Some("yes")), "W").state, PositionState::Open);
    }

    #[test]
    fn only_a_claimable_row_offers_a_claim_and_none_can_be_sold() {
        let claimable = to_position(row("yes", "38", "resolved", true, false, Some("yes")), "WALLET");
        assert_eq!(claimable.claim_ref.as_deref(), Some("claim:WALLET:MKT"));
        assert_eq!(claimable.payout_usdc, Some(dec("38")));
        let open = to_position(row("yes", "38", "primary", false, false, None), "WALLET");
        assert!(open.claim_ref.is_none() && open.payout_usdc.is_none());
        assert!(!claimable.can_sell && !open.can_sell, "panta's API has no sell");
    }

    #[test]
    fn what_panta_does_not_report_stays_unknown() {
        let p = to_position(row("YES", "5", "primary", false, false, None), "W");
        assert_eq!((p.cost_usdc, p.value_usdc, p.pnl_usdc, p.avg_price_usdc), (None, None, None, None));
        assert_eq!(p.side, "yes", "side is normalised");
    }
}
