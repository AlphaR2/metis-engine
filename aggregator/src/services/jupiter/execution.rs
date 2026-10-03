//! Jupiter order placement.
//!
//! `POST /orders` returns an unsigned transaction. The caller signs it and submits it to Solana
//! **directly** — there is no execute endpoint, and neither Jupiter nor Metis broadcasts. Status
//! is then polled from `GET /orders/status/{orderPubkey}`.
//!
//! Two guards Jupiter's own documentation demands, both enforced here rather than trusted to the
//! caller:
//!
//! - **Trading has to be live.** `GET /trading-status` must report `trading_active`.
//! - **The price must be fresh.** Jupiter: *"Prices move. Re-fetch `GET /markets/{marketId}`
//!   immediately before placing an order."* Our catalog is refreshed on a ten-minute cycle, so the
//!   cached price is never what an order is built against.
//!
//! ## On slippage
//!
//! `docs/mvp.md` §1.5 locks a caller-supplied bound for the in-house AMM, so that a race which
//! moves the price past the user's tolerance reverts rather than filling badly. **Jupiter offers no
//! equivalent request parameter** — checked against its OpenAPI spec: `maxBuyPriceUsd`,
//! `slippageBps` and `maxSlippageBps` are all *response* fields it computes.
//!
//! A ceiling therefore does exist and is encoded in the transaction, but it is Jupiter's, not the
//! user's. Those figures are surfaced on the built transaction so they can be shown before signing.
//! Pretending we had set the bound would be the dishonest option.

use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::execution::{
    ActionStatus, BuyIntent, ConfirmOutcome, CostLine, IntentKind, OrderPreview, TransactionCost,
    UnsignedTransaction,
};
use crate::models::venue::jupiter::{
    JupCreateOrderRequest, JupCreateOrderResponse, JupOrderStatus, JupTradingStatus,
    micro_usd_to_decimal, usdc_to_base_units,
};
use crate::utils::http::parse_json_with_diagnostics;
use crate::utils::time::now_unix;

/// Jupiter's documented floor. Rejected before building rather than after the venue charges for
/// a transaction that could never have worked.
const MIN_DEPOSIT_USDC: i64 = 5;

/// How long a Solana blockhash stays usable, conservatively.
///
/// Jupiter reports expiry as `lastValidBlockHeight`, which cannot be turned into a wall-clock time
/// without asking the chain — and this service has no RPC connection. So this is an estimate, and
/// the block height is passed through for the frontend, which does have RPC, to resolve exactly.
const ASSUMED_BLOCKHASH_SECS: i64 = 60;

/// Checks the venue is accepting orders at all.
async fn assert_trading_live(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
) -> Result<(), AdapterError> {
    let response = client
        .get(format!("{base_url}/trading-status"))
        .headers(crate::utils::http::api_key_headers("x-api-key", api_key))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AdapterError::Venue(e.to_string()))?;

    let status: JupTradingStatus =
        parse_json_with_diagnostics(response, "jupiter trading-status").await.map_err(AdapterError::Parse)?;

    if !status.trading_active {
        // Not a failure of ours or theirs - the venue is deliberately paused, and a user should be
        // told that rather than shown a generic error.
        return Err(AdapterError::InvalidRequest(
            "Jupiter has trading paused right now, so an order cannot be placed.".to_string(),
        ));
    }
    Ok(())
}

/// Builds the cost breakdown from what Jupiter priced.
///
/// The total is `orderCostUsd` where Jupiter gives one, falling back to what the caller asked to
/// spend. Fees are listed separately rather than folded in, because a trader comparing venues needs
/// to see them.
pub(super) fn cost_from(order: &crate::models::venue::jupiter::JupOrderDetail, requested: Decimal) -> TransactionCost {
    let total = order.order_cost_usd.map(micro_usd_to_decimal).unwrap_or(requested);

    let mut lines = Vec::new();
    if let Some(fee) = order.estimated_protocol_fee_usd.filter(|f| *f > 0).map(micro_usd_to_decimal) {
        lines.push(CostLine {
            label: "Protocol fee".to_string(),
            amount_usdc: fee,
            note: Some("Jupiter's own fee for routing this order.".to_string()),
        });
    }
    if let Some(fee) = order.estimated_venue_fee_usd.filter(|f| *f > 0).map(micro_usd_to_decimal) {
        lines.push(CostLine {
            label: "Venue fee".to_string(),
            amount_usdc: fee,
            note: Some("Charged by the underlying market (Polymarket or Kalshi).".to_string()),
        });
    }
    if let Some(price) = order.max_buy_price_usd.map(micro_usd_to_decimal) {
        lines.push(CostLine {
            label: "Worst price per contract".to_string(),
            amount_usdc: price,
            note: Some(
                "The ceiling this order can fill at. Set by Jupiter, not by you — it accepts no \
                 slippage limit on the request."
                    .to_string(),
            ),
        });
    }
    TransactionCost { total_usdc: total, lines }
}

/// What the buyer receives, for the pre-sign preview.
pub(super) fn preview_from(order: &crate::models::venue::jupiter::JupOrderDetail) -> OrderPreview {
    OrderPreview {
        contracts: order.contracts_decimal.clone(),
        avg_price_usdc: order.new_avg_price_usd.map(micro_usd_to_decimal),
        payout_if_win_usdc: order.new_payout_usd.map(micro_usd_to_decimal),
        slippage_bps: order.slippage_bps,
        max_slippage_bps: order.max_slippage_bps,
    }
}

/// Builds a buy, after checking trading is live and re-pricing against the venue.
pub async fn build_buy(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    intent: &BuyIntent,
) -> Result<UnsignedTransaction, AdapterError> {
    if intent.amount_usdc < Decimal::from(MIN_DEPOSIT_USDC) {
        return Err(AdapterError::BelowMinimum {
            venue: "Jupiter",
            minimum_usdc: MIN_DEPOSIT_USDC.to_string(),
            amount_usdc: intent.amount_usdc.to_string(),
        });
    }
    let is_yes = match intent.side.to_lowercase().as_str() {
        "yes" => true,
        "no" => false,
        other => {
            return Err(AdapterError::InvalidRequest(format!(
                "unknown side '{other}' — Jupiter markets are yes/no"
            )));
        }
    };

    assert_trading_live(client, base_url, api_key).await?;

    // Building the order is itself the re-price: Jupiter quotes against live state at this moment,
    // and returns what it will actually cost. That figure - not our cached catalog price - is what
    // the caller is shown before signing.
    let body = JupCreateOrderRequest {
        owner_pubkey: intent.wallet.clone(),
        market_id: intent.venue_market_id.clone(),
        is_yes,
        is_buy: true,
        deposit_amount: usdc_to_base_units(intent.amount_usdc).to_string(),
        deposit_mint: None,
    };

    let response = client
        .post(format!("{base_url}/orders"))
        .headers(crate::utils::http::api_key_headers("x-api-key", api_key))
        .json(&body)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AdapterError::Venue(e.to_string()))?;

    let built: JupCreateOrderResponse =
        parse_json_with_diagnostics(response, "jupiter create order").await.map_err(AdapterError::Parse)?;

    let signers = if built.required_signers.is_empty() {
        vec![intent.wallet.clone()]
    } else {
        built.required_signers.clone()
    };

    Ok(UnsignedTransaction {
        venue_id: "jupiter".to_string(),
        kind: IntentKind::BuyShares,
        transaction: built.transaction,
        venue_ref: built.order.order_pubkey.clone(),
        expires_at: now_unix() + ASSUMED_BLOCKHASH_SECS,
        required_signers: signers,
        last_valid_block_height: built.tx_meta.as_ref().and_then(|m| m.last_valid_block_height),
        cost: cost_from(&built.order, intent.amount_usdc),
        preview: Some(preview_from(&built.order)),
    })
}

/// Maps Jupiter's order status onto ours.
fn map_status(raw: &str) -> ActionStatus {
    match raw.to_lowercase().as_str() {
        "filled" => ActionStatus::Confirmed,
        // A real settled outcome on an order book, not an error and not still in flight.
        "partiallyfilled" | "partially_filled" => ActionStatus::PartiallyFilled,
        "failed" => ActionStatus::Failed,
        // "created" means accepted and waiting to fill.
        _ => ActionStatus::Submitted,
    }
}

/// Polls an order's status. Read-only, so it is safe to call as often as a caller likes and needs
/// no signature — which is why `confirm` and status polling are the same call here.
pub async fn order_status(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    order_pubkey: &str,
) -> Result<ConfirmOutcome, AdapterError> {
    let response = client
        .get(crate::utils::http::venue_url(base_url, &["orders", "status", order_pubkey])?)
        .headers(crate::utils::http::api_key_headers("x-api-key", api_key))
        .send()
        .await?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        // Jupiter has not seen it. Usually means the transaction was never broadcast, or has not
        // landed yet - not that it failed.
        return Ok(ConfirmOutcome {
            venue_ref: order_pubkey.to_string(),
            status: ActionStatus::Submitted,
            venue_market_id: None,
            message: Some("Jupiter has no record of this order yet.".to_string()),
        });
    }

    let response = response.error_for_status().map_err(|e| AdapterError::Venue(e.to_string()))?;
    let status: JupOrderStatus =
        parse_json_with_diagnostics(response, "jupiter order status").await.map_err(AdapterError::Parse)?;

    Ok(ConfirmOutcome {
        venue_ref: order_pubkey.to_string(),
        status: map_status(&status.status),
        venue_market_id: status.market_id,
        message: status.message.or(Some(status.status)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::venue::jupiter::JupOrderDetail;

    fn detail() -> JupOrderDetail {
        JupOrderDetail {
            order_pubkey: "Order1".to_string(),
            contracts_decimal: Some("100".to_string()),
            max_buy_price_usd: Some(520_000),
            order_cost_usd: Some(50_000_000),
            estimated_protocol_fee_usd: Some(250_000),
            estimated_venue_fee_usd: Some(0),
            estimated_total_fee_usd: Some(250_000),
            slippage_bps: Some(12),
            max_slippage_bps: Some(100),
            new_avg_price_usd: Some(500_000),
            new_payout_usd: Some(100_000_000),
        }
    }

    #[test]
    fn micro_usd_converts_exactly() {
        assert_eq!(micro_usd_to_decimal(520_000), "0.52".parse::<Decimal>().unwrap());
        assert_eq!(micro_usd_to_decimal(50_000_000), Decimal::from(50));
    }

    #[test]
    fn usdc_converts_to_base_units_without_rounding_up() {
        assert_eq!(usdc_to_base_units(Decimal::from(50)), 50_000_000);
        // Truncates rather than rounds: spending more than the user asked for is not ours to do.
        assert_eq!(usdc_to_base_units("10.0000009".parse().unwrap()), 10_000_000);
    }

    #[test]
    fn the_cost_uses_jupiters_own_total_not_the_requested_amount() {
        let cost = cost_from(&detail(), Decimal::from(49));
        assert_eq!(cost.total_usdc, Decimal::from(50));
    }

    #[test]
    fn a_zero_fee_line_is_omitted_rather_than_shown_as_zero() {
        let cost = cost_from(&detail(), Decimal::from(50));
        assert!(!cost.lines.iter().any(|l| l.label == "Venue fee"));
        assert!(cost.lines.iter().any(|l| l.label == "Protocol fee"));
    }

    #[test]
    fn the_price_ceiling_is_surfaced_and_attributed_to_jupiter() {
        // The locked slippage requirement cannot be met on this venue, so the UI must say whose
        // ceiling it is rather than imply the user set it.
        let cost = cost_from(&detail(), Decimal::from(50));
        let line = cost.lines.iter().find(|l| l.label == "Worst price per contract").unwrap();
        assert_eq!(line.amount_usdc, "0.52".parse::<Decimal>().unwrap());
        assert!(line.note.as_ref().unwrap().contains("not by you"));
    }

    #[test]
    fn the_preview_reports_what_the_buyer_receives() {
        let p = preview_from(&detail());
        assert_eq!(p.contracts.as_deref(), Some("100"));
        assert_eq!(p.avg_price_usdc, Some("0.5".parse().unwrap()));
        // 100 contracts at $1 each if it wins - the number that makes $0.50 meaningful.
        assert_eq!(p.payout_if_win_usdc, Some(Decimal::from(100)));
        assert_eq!(p.max_slippage_bps, Some(100));
        assert!(p.contracts.is_some() && p.avg_price_usdc.is_some());
    }

    #[test]
    fn an_order_jupiter_priced_thinly_still_previews_what_it_can() {
        let mut d = detail();
        d.contracts_decimal = None;
        d.new_payout_usd = None;
        let p = preview_from(&d);
        assert!(p.contracts.is_none());
        assert!(p.avg_price_usdc.is_some(), "a partial preview is still worth showing");
    }

    #[test]
    fn order_statuses_map_onto_our_own() {
        assert_eq!(map_status("filled"), ActionStatus::Confirmed);
        assert_eq!(map_status("partiallyfilled"), ActionStatus::PartiallyFilled);
        assert_eq!(map_status("failed"), ActionStatus::Failed);
        assert_eq!(map_status("created"), ActionStatus::Submitted);
    }

    #[test]
    fn a_partial_fill_is_settled_not_still_pending() {
        // It is an outcome, not a stage - the book gave what it had. Treating it as pending would
        // leave a caller polling forever.
        assert_eq!(map_status("partiallyfilled"), ActionStatus::PartiallyFilled);
    }
}
