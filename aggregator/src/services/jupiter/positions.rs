//! Jupiter positions, claims and sells.
//!
//! - `GET /positions?ownerPubkey=` lists what a wallet holds, with Jupiter's own pnl and claim flags
//! - `POST /positions/{pubkey}/claim` returns an unsigned claim transaction
//! - `DELETE /positions/{pubkey}` returns an unsigned **sell order**, which like a buy only opens
//!   an order account; a keeper fills it afterwards, so it is tracked with the same order status
//!
//! A claim has no order of its own to poll, so its `venue_ref` carries the owner and the position
//! (`claim:<owner>:<position>`) and its status is read back from the position's `claimed` flag.

use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::execution::{
    ActionStatus, ClaimIntent, ConfirmOutcome, IntentKind, SellIntent, TransactionCost, UnsignedTransaction,
    claim_ref, parse_claim_ref,
};
use crate::models::position::{Position, PositionState};
use crate::models::venue::jupiter::{
    JupClaimResponse, JupCreateOrderResponse, JupPosition, JupPositionsResponse,
};
use crate::utils::http::{check_status, parse_json_with_diagnostics, venue_url};
use crate::utils::time::now_unix;

/// Positions per request, and the most pages read for one wallet. A wallet with more than
/// `PAGE * MAX_PAGES` positions gets a truncated list, which is reported as a failure rather than
/// returned as if complete.
const PAGE: usize = 100;
const MAX_PAGES: usize = 10;

/// Same conservative estimate the buy uses: Jupiter reports `lastValidBlockHeight`, and the client,
/// which has RPC, resolves the exact window.
const ASSUMED_BLOCKHASH_SECS: i64 = 60;

/// Where a Jupiter position stands.
fn state_of(position: &JupPosition, now: i64) -> PositionState {
    if position.claimed {
        PositionState::Claimed
    } else if position.claimable {
        PositionState::Claimable
    } else if position.settlement_date.is_some_and(|settled| settled > 0 && settled <= now)
        && position.payout.unwrap_or_default().is_zero()
    {
        // Settled, nothing to collect, and not claimable: it settled against this side.
        PositionState::Lost
    } else {
        PositionState::Open
    }
}

/// Maps one Jupiter position into the venue-neutral shape.
pub fn to_position(position: JupPosition, wallet: &str, now: i64) -> Position {
    let state = state_of(&position, now);
    let contracts = position.contracts();
    let side = if position.is_yes { "yes" } else { "no" };
    Position {
        venue_id: "jupiter".to_string(),
        venue_position_id: Some(position.pubkey.clone()),
        claim_ref: (state == PositionState::Claimable).then(|| claim_ref(wallet, &position.pubkey)),
        can_sell: state == PositionState::Open && contracts > Decimal::ZERO,
        title: position.title(),
        venue_market_id: position.market_id,
        side: side.to_string(),
        contracts,
        avg_price_usdc: position.avg_price,
        cost_usdc: position.total_cost,
        value_usdc: position.value,
        pnl_usdc: position.pnl,
        value_is_estimate: false,
        payout_usdc: position.payout.filter(|p| !p.is_zero()),
        state,
        phase: None,
        open_orders: position.open_orders,
        event_id: None,
        event_title: None,
        updated_at: position.updated_at,
    }
}

/// One page of a wallet's positions. Returns the rows and whether more remain.
async fn positions_page(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    wallet: &str,
    start: usize,
) -> Result<(Vec<JupPosition>, bool), AdapterError> {
    let response = client
        .get(venue_url(base_url, &["positions"])?)
        .header("x-api-key", api_key)
        .query(&[
            ("ownerPubkey", wallet.to_string()),
            ("start", start.to_string()),
            ("end", (start + PAGE).to_string()),
        ])
        .send()
        .await?;
    let response = check_status(response, "jupiter positions").await?;
    let page: JupPositionsResponse =
        parse_json_with_diagnostics(response, "jupiter positions").await.map_err(AdapterError::Parse)?;
    Ok((page.data, page.pagination.has_next))
}

/// Every position the wallet holds on Jupiter.
///
/// A wallet with more than the page cap gets an error, not a silently short list: a portfolio that
/// hides positions is worse than one that says it could not load.
pub async fn list_positions(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    wallet: &str,
) -> Result<Vec<Position>, AdapterError> {
    let now = now_unix();
    let mut out = Vec::new();
    for page in 0..MAX_PAGES {
        let (rows, has_next) = positions_page(client, base_url, api_key, wallet, page * PAGE).await?;
        // The venue was asked for this wallet only. A row that names another owner means the
        // filter was ignored, and showing it would present someone else's holdings as this
        // wallet's, so the whole read is refused.
        if rows.iter().any(|row| row.owner_pubkey.as_deref().is_some_and(|owner| owner != wallet)) {
            return Err(AdapterError::Venue("jupiter returned positions owned by a different wallet".to_string()));
        }
        out.extend(rows.into_iter().map(|row| to_position(row, wallet, now)));
        if !has_next {
            return Ok(out);
        }
    }
    Err(AdapterError::Venue(format!(
        "jupiter: this wallet has more than {} positions, so the list would be incomplete",
        PAGE * MAX_PAGES
    )))
}

/// Builds a claim for a winning position. Does not sign and does not broadcast.
pub async fn build_claim(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    intent: &ClaimIntent,
) -> Result<UnsignedTransaction, AdapterError> {
    if intent.kind != IntentKind::ClaimWinnings {
        return Err(AdapterError::Unsupported { venue: "jupiter", action: "creator-fee claims" });
    }
    let (owner, position) = parse_claim_ref(&intent.venue_ref).ok_or_else(|| {
        AdapterError::InvalidRequest("venue_ref is not a jupiter claim reference - use claim_ref from /v1/positions".to_string())
    })?;
    // The wallet asking must be the position's owner. Jupiter also checks, but a claim built for
    // someone else's position is never something this API should produce.
    if owner != intent.wallet {
        return Err(AdapterError::InvalidRequest("this claim reference belongs to a different wallet".to_string()));
    }

    let response = client
        .post(venue_url(base_url, &["positions", position, "claim"])?)
        .header("x-api-key", api_key)
        .json(&serde_json::json!({ "ownerPubkey": owner }))
        .send()
        .await?;
    let response = check_status(response, "jupiter claim").await?;
    let built: JupClaimResponse =
        parse_json_with_diagnostics(response, "jupiter claim").await.map_err(AdapterError::Parse)?;

    let payout = built.position.as_ref().and_then(|p| p.payout);
    Ok(UnsignedTransaction {
        venue_id: "jupiter".to_string(),
        kind: IntentKind::ClaimWinnings,
        transaction: built.transaction,
        venue_ref: claim_ref(owner, position),
        expires_at: now_unix() + ASSUMED_BLOCKHASH_SECS,
        required_signers: vec![intent.wallet.clone()],
        last_valid_block_height: built.tx_meta.and_then(|m| m.last_valid_block_height),
        // Nothing leaves the wallet: a claim only pays in. What it pays is in the preview.
        cost: TransactionCost { total_usdc: Decimal::ZERO, lines: vec![] },
        preview: Some(crate::models::execution::OrderPreview {
            contracts: built.position.and_then(|p| p.contracts_decimal),
            payout_if_win_usdc: payout,
            ..Default::default()
        }),
    })
}

/// Builds a sell that exits a position. Does not sign and does not broadcast.
///
/// Jupiter answers with the same shape as a buy: an order account to open, filled afterwards by a
/// keeper. So the reference is the order, and `order_status` tracks it.
pub async fn build_sell(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    intent: &SellIntent,
) -> Result<UnsignedTransaction, AdapterError> {
    let response = client
        .delete(venue_url(base_url, &["positions", &intent.venue_ref])?)
        .header("x-api-key", api_key)
        .json(&serde_json::json!({ "ownerPubkey": intent.wallet }))
        .send()
        .await?;
    let response = check_status(response, "jupiter sell").await?;
    let built: JupCreateOrderResponse =
        parse_json_with_diagnostics(response, "jupiter sell").await.map_err(AdapterError::Parse)?;

    let signers = if built.required_signers.is_empty() { vec![intent.wallet.clone()] } else { built.required_signers.clone() };
    let mut cost = super::execution::cost_from(&built.order, Decimal::ZERO);
    // A sell pays the wallet, so nothing leaves it beyond fees taken from proceeds. Jupiter's
    // `orderCostUsd` on a sell is not a wallet outflow, so it is not reported as one; the
    // client's balance check treats a sell as one where USDC must not decrease.
    cost.total_usdc = Decimal::ZERO;
    Ok(UnsignedTransaction {
        venue_id: "jupiter".to_string(),
        kind: IntentKind::SellShares,
        transaction: built.transaction,
        venue_ref: built.order.order_pubkey.clone(),
        expires_at: now_unix() + ASSUMED_BLOCKHASH_SECS,
        required_signers: signers,
        last_valid_block_height: built.tx_meta.as_ref().and_then(|m| m.last_valid_block_height),
        cost,
        preview: Some(super::execution::preview_from(&built.order)),
    })
}

/// A claim's status, read back from the position. Read-only, so safe to poll.
///
/// Jupiter has no order to poll for a claim, so this looks for the position and checks `claimed`.
/// It can confirm success; it cannot tell that a claim failed on-chain (that is the client's
/// signature check against the chain), so an unclaimed position is reported as still waiting.
pub async fn claim_status(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    venue_ref: &str,
) -> Result<ConfirmOutcome, AdapterError> {
    let (owner, position) = parse_claim_ref(venue_ref)
        .ok_or_else(|| AdapterError::InvalidRequest(format!("'{venue_ref}' is not a jupiter claim reference")))?;

    for page in 0..MAX_PAGES {
        let (rows, has_next) = positions_page(client, base_url, api_key, owner, page * PAGE).await?;
        if let Some(found) = rows.iter().find(|row| row.pubkey == position) {
            let (status, message) = if found.claimed {
                (ActionStatus::Confirmed, "Claimed.")
            } else {
                (ActionStatus::Submitted, "The claim has not been recorded yet.")
            };
            return Ok(ConfirmOutcome {
                venue_ref: venue_ref.to_string(),
                status,
                venue_market_id: Some(found.market_id.clone()),
                message: Some(message.to_string()),
            });
        }
        if !has_next {
            break;
        }
    }
    Ok(ConfirmOutcome {
        venue_ref: venue_ref.to_string(),
        status: ActionStatus::Submitted,
        venue_market_id: None,
        message: Some("Jupiter has no record of this position yet.".to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WALLET: &str = "Wallet1111111111111111111111111111111111111";

    fn position(json: &str) -> JupPosition {
        serde_json::from_str(json).unwrap()
    }

    fn mapped(json: &str) -> Position {
        to_position(position(json), WALLET, 1_800_000_000)
    }

    #[test]
    fn a_claimable_position_offers_a_claim_and_no_sale() {
        let p = mapped(r#"{"pubkey":"P","marketId":"M","isYes":true,"contractsDecimal":"10","claimable":true,"claimed":false,"payoutUsd":"10000000"}"#);
        assert_eq!(p.state, PositionState::Claimable);
        assert_eq!(p.claim_ref.as_deref(), Some(format!("claim:{WALLET}:P").as_str()));
        assert!(!p.can_sell);
        assert_eq!(p.payout_usdc, Some(Decimal::from(10)));
    }

    #[test]
    fn a_claimed_position_offers_nothing() {
        let p = mapped(r#"{"pubkey":"P","marketId":"M","contractsDecimal":"10","claimable":false,"claimed":true}"#);
        assert_eq!(p.state, PositionState::Claimed);
        assert!(p.claim_ref.is_none() && !p.can_sell);
    }

    #[test]
    fn an_open_position_can_be_sold_but_not_claimed() {
        let p = mapped(r#"{"pubkey":"P","marketId":"M","isYes":false,"contractsDecimal":"3","valueUsd":"1500000"}"#);
        assert_eq!((p.state, p.side.as_str()), (PositionState::Open, "no"));
        assert!(p.can_sell && p.claim_ref.is_none());
        assert_eq!(p.value_usdc, Some("1.5".parse().unwrap()));
    }

    #[test]
    fn a_settled_market_with_no_payout_is_lost_not_open() {
        let p = mapped(r#"{"pubkey":"P","marketId":"M","contractsDecimal":"3","claimable":false,"claimed":false,"settlementDate":1700000000,"payoutUsd":"0"}"#);
        assert_eq!(p.state, PositionState::Lost);
        assert!(!p.can_sell && p.claim_ref.is_none());
    }

    #[test]
    fn a_future_settlement_date_does_not_make_it_lost() {
        let p = mapped(r#"{"pubkey":"P","marketId":"M","contractsDecimal":"3","settlementDate":1900000000}"#);
        assert_eq!(p.state, PositionState::Open);
    }

    #[test]
    fn a_zero_share_position_cannot_be_sold() {
        let p = mapped(r#"{"pubkey":"P","marketId":"M","contractsDecimal":"0"}"#);
        assert!(!p.can_sell);
    }

    #[test]
    fn unknown_values_stay_unknown() {
        let p = mapped(r#"{"pubkey":"P","marketId":"M","contractsDecimal":"1"}"#);
        assert_eq!((p.value_usdc, p.pnl_usdc, p.cost_usdc), (None, None, None));
    }

    #[test]
    fn a_row_owned_by_another_wallet_is_recognisable() {
        let row: JupPosition = serde_json::from_str(r#"{"pubkey":"P","marketId":"M","ownerPubkey":"SOMEONE_ELSE"}"#).unwrap();
        assert!(row.owner_pubkey.as_deref().is_some_and(|owner| owner != WALLET));
        let mine: JupPosition = serde_json::from_str(&format!(r#"{{"pubkey":"P","marketId":"M","ownerPubkey":"{WALLET}"}}"#)).unwrap();
        assert!(!mine.owner_pubkey.as_deref().is_some_and(|owner| owner != WALLET));
    }
}
