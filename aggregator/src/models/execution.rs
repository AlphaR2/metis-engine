//! Venue-neutral shapes for actions that need a signature.
//!
//! Buying shares on Jupiter, creating a market on Panta, and claiming winnings or creator fees are
//! four different products, but on the wire they are one mechanism:
//!
//! ```text
//! the venue builds an unsigned transaction
//!   → the user's wallet signs it
//!   → it is broadcast to Solana
//!   → the venue is told the signature
//! ```
//!
//! So they share one type here rather than four parallel ones. **Metis never signs and never holds
//! a key** - it hands back bytes the caller's wallet is free to inspect, sign, or refuse. That is
//! the same non-custodial property the read-only aggregator already has, and nothing in this module
//! weakens it.
//!
//! Money is `Decimal` throughout, matching the rest of the service: exact fixed-point, serialized
//! as a string, never `f64`.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What a signature is being requested for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    /// Buy shares in an existing market.
    BuyShares,
    /// Sell shares already held, before the market resolves.
    SellShares,
    /// Create a new market on a venue that allows it.
    CreateMarket,
    /// Collect a payout from a resolved market.
    ClaimWinnings,
    /// Collect the fees a market has accrued for whoever created it.
    ClaimCreatorFees,
}


/// One line of what an action costs.
///
/// Itemized rather than a single total because the parts mean different things and the user is
/// entitled to see which is which - Panta's creation fee, for instance, splits into liquidity that
/// seeds the new market and revenue that goes to the platform. A lump sum would hide that.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CostLine {
    /// Short label, e.g. "Liquidity injection".
    pub label: String,
    #[schema(value_type = String)]
    pub amount_usdc: Decimal,
    /// One line explaining what this part is for, shown next to the number.
    pub note: Option<String>,
}

/// What an action costs, in full.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TransactionCost {
    /// What leaves the wallet in total. Not derived from `lines` - it is the venue's own figure,
    /// and if the two disagree that is a discrepancy worth surfacing rather than hiding behind a
    /// recomputed sum.
    #[schema(value_type = String)]
    pub total_usdc: Decimal,
    pub lines: Vec<CostLine>,
}

impl TransactionCost {
    /// Whether the itemized lines actually add up to the stated total.
    ///
    /// A mismatch is not automatically an error - a venue may omit a line - but it is something a
    /// caller should be able to notice rather than silently present a breakdown that doesn't
    /// reconcile.
    // Invariant check the venue cost-breakdown tests assert; nothing in the serving path needs it.
    #[cfg(test)]
    pub fn lines_reconcile(&self) -> bool {
        if self.lines.is_empty() {
            return true;
        }
        self.lines.iter().map(|l| l.amount_usdc).sum::<Decimal>() == self.total_usdc
    }
}

/// What the signer actually gets, alongside what it costs.
///
/// `TransactionCost` answers "what leaves my wallet". This answers "and what do I get for it" —
/// the other half of an informed decision, and the half a cost breakdown alone cannot express.
///
/// `docs/research.md` §3.3 makes previewing before signing a locked decision: the projected
/// outcome has to be on screen before a signature is requested, not after.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct OrderPreview {
    /// Contracts (shares) acquired, as a decimal string.
    pub contracts: Option<String>,
    /// The average price per contract the resulting position would carry.
    #[schema(value_type = Option<String>)]
    pub avg_price_usdc: Option<Decimal>,
    /// What the position pays out if it wins. The number that makes a price meaningful.
    #[schema(value_type = Option<String>)]
    pub payout_if_win_usdc: Option<Decimal>,
    /// Expected slippage, in basis points.
    pub slippage_bps: Option<i64>,
    /// The slippage ceiling encoded in the transaction.
    ///
    /// On Jupiter this is **the venue's** ceiling, not one the caller chose — it accepts no
    /// slippage parameter on the request. A UI must attribute it honestly rather than present it
    /// as a tolerance the user set.
    pub max_slippage_bps: Option<i64>,
}


/// A venue-built transaction waiting for the user's signature.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UnsignedTransaction {
    pub venue_id: String,
    pub kind: IntentKind,
    /// Base64 `VersionedTransaction`, exactly as the venue produced it.
    ///
    /// Passed through untouched. Both venues fingerprint or re-verify what they built, so altering
    /// it here would be both detectable and pointless - and a proxy that edits transactions before
    /// they are signed is precisely the thing a non-custodial design exists to rule out.
    pub transaction: String,
    /// The venue's handle for this pending action - Panta's `createId`, Jupiter's `orderPubkey`.
    /// This is what `confirm` is keyed on, and what makes a retry idempotent.
    pub venue_ref: String,
    /// Unix seconds after which this is no longer submittable, taking whichever of the venue's
    /// session or blockhash expires first.
    ///
    /// These windows are short - Panta's blockhash is around 60 seconds - so this is not a
    /// formality. A caller that sits on the transaction must rebuild rather than submit.
    pub expires_at: i64,
    /// Public keys that must sign. Usually just the user's wallet.
    pub required_signers: Vec<String>,
    /// The last Solana block height at which the embedded blockhash is still valid, when the venue
    /// tells us.
    ///
    /// `expires_at` above is a wall-clock estimate, because a block height cannot be converted to a
    /// time without asking the chain — and this service has no RPC connection. The frontend does,
    /// so passing this through lets it compute the true remaining window rather than trusting our
    /// approximation. Where they disagree, the block height is the authority.
    #[serde(default)]
    pub last_valid_block_height: Option<u64>,
    /// What this costs, itemized. Shown before a signature is requested, never after - see
    /// `docs/research.md` §3.3, where previewing before signing is a locked decision.
    pub cost: TransactionCost,
    /// What the signer receives. Absent for an action where "what do I get" isn't meaningful, such
    /// as creating a market.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<OrderPreview>,
}

impl UnsignedTransaction {
    /// Seconds left before expiry, saturating at zero. Drives the countdown a signing UI shows.
    pub fn seconds_remaining(&self, now: i64) -> i64 {
        if self.expires_at == 0 {
            return 0;
        }
        (self.expires_at - now).max(0)
    }
}

/// A request to buy shares in an existing market.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct BuyIntent {
    pub venue_id: String,
    pub venue_market_id: String,
    /// The venue's own word for the side - "yes"/"no" for Panta and Jupiter.
    pub side: String,
    /// How much to spend, in USDC.
    #[schema(value_type = String)]
    pub amount_usdc: Decimal,
    /// The wallet that will sign and be charged.
    pub wallet: String,
    /// Minimum shares the caller will accept, computed from a fresh quote.
    ///
    /// `docs/mvp.md` §1.5 locks this for the in-house AMM - a race that moves the price past the
    /// user's tolerance should revert rather than fill badly - and the same protection is wanted
    /// on external venues. `None` where a venue offers no equivalent bound, which is a real gap
    /// to state plainly rather than quietly accept.
    #[schema(value_type = Option<String>)]
    pub min_shares_out: Option<Decimal>,
}

/// A request to create a new market.
///
/// Field names follow Panta's create API, the only venue that currently supports this.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateMarketIntent {
    pub venue_id: String,
    /// The question being asked. This is the market's whole point; everything else is metadata.
    pub question: String,
    /// What counts as a win, in full.
    pub resolution_rule: String,
    /// Where the answer will be read from. Must be non-empty - a market with no source of truth
    /// cannot be resolved honestly.
    pub sources_of_truth: Vec<String>,
    /// Canonical category, translated to the venue's own vocabulary by its adapter.
    pub category: String,
    /// Unix seconds. Venues impose a minimum delay ahead of now.
    pub start_time: i64,
    /// When betting closes. Must be after `start_time`.
    pub end_time: i64,
    /// When the outcome is known. Must be after `end_time`.
    pub resolution_time: i64,
    /// Catalog image. Mandatory on Panta.
    pub image_url: String,
    /// The wallet that pays the creation fee and will earn the creator fees.
    pub wallet: String,
    /// Defaults to `question` when absent.
    pub title: Option<String>,
    pub description: Option<String>,
    pub region: Option<String>,
    /// Venue-specific extras, for venues whose markets need more than the fields above. Metis takes
    /// `oracle` (the football oracle account, also accepted as a `sources_of_truth` entry
    /// `oracle:<address>`), `condition` (`home_win`, `away_win`, `draw`, `both_teams_score` or
    /// `total_goals_over:<n>`), `label_yes`, `label_no`, `rubric_no`, `league`, and `step`:
    /// `create` (default) opens the market, then `metadata` (with `market`) writes its text.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub venue_params: Option<serde_json::Value>,
}

/// `venue_ref` for a pending claim, on any venue: `claim:<owner>:<what is claimed>`.
///
/// A claim has no order of its own to poll, so its reference carries the owner and the thing
/// claimed (Jupiter's position account, Panta's market). That is enough to read the answer back
/// from the venue's positions, with no stored state.
pub fn claim_ref(owner: &str, target: &str) -> String {
    format!("claim:{owner}:{target}")
}

/// `(owner, target)` from a claim reference, if it is one.
pub fn parse_claim_ref(venue_ref: &str) -> Option<(&str, &str)> {
    let rest = venue_ref.strip_prefix("claim:")?;
    let (owner, target) = rest.split_once(':')?;
    (!owner.is_empty() && !target.is_empty() && !target.contains(':')).then_some((owner, target))
}

/// A request to sell a position before its market resolves.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SellIntent {
    pub venue_id: String,
    /// The venue's id for the position being sold - `venue_position_id` from `GET /v1/positions`.
    pub venue_ref: String,
    /// The wallet that owns the position, signs, and receives the proceeds.
    pub wallet: String,
}

/// A request to claim a payout or accrued creator fees.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ClaimIntent {
    pub venue_id: String,
    pub kind: IntentKind,
    /// What is being claimed against: `claim_ref` from `GET /v1/positions`.
    pub venue_ref: String,
    pub wallet: String,
}

/// How a venue answered once it was told the transaction had been broadcast.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConfirmOutcome {
    pub venue_ref: String,
    pub status: ActionStatus,
    /// Set when the action produced something addressable - a created market's id, or the market
    /// an order was filled against.
    pub venue_market_id: Option<String>,
    /// The venue's own message, kept verbatim. Both venues return named failure codes
    /// (`CREATE_EXPIRED`, `TX_FEE_MISMATCH`, ...) that say far more than "it failed".
    pub message: Option<String>,
}

/// Where a submitted action has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    /// Built and handed back, not yet broadcast.
    Built,
    /// Broadcast, awaiting the venue's confirmation.
    Submitted,
    /// Accepted and complete.
    Confirmed,
    /// Accepted but only partly filled - a real outcome on an order book, not an error.
    PartiallyFilled,
    /// The venue rejected it, or the transaction failed on-chain.
    Failed,
    /// The venue's window closed before the transaction was broadcast.
    Expired,
}


#[cfg(test)]
mod tests {
    use super::*;

    fn tx(expires_at: i64) -> UnsignedTransaction {
        UnsignedTransaction {
            venue_id: "panta".to_string(),
            kind: IntentKind::CreateMarket,
            transaction: "base64==".to_string(),
            venue_ref: "create_1".to_string(),
            expires_at,
            required_signers: vec!["Wallet1".to_string()],
            last_valid_block_height: None,
            cost: TransactionCost { total_usdc: Decimal::new(25, 0), lines: Vec::new() },
            preview: None,
        }
    }

    const NOW: i64 = 1_000_000;

    #[test]
    fn seconds_remaining_never_goes_negative() {
        assert_eq!(tx(NOW + 45).seconds_remaining(NOW), 45);
        assert_eq!(tx(NOW - 45).seconds_remaining(NOW), 0);
    }

    #[test]
    fn an_itemised_cost_that_adds_up_reconciles() {
        let cost = TransactionCost {
            total_usdc: Decimal::new(25, 0),
            lines: vec![
                CostLine { label: "Liquidity".into(), amount_usdc: Decimal::new(20, 0), note: None },
                CostLine { label: "Platform".into(), amount_usdc: Decimal::new(5, 0), note: None },
            ],
        };
        assert!(cost.lines_reconcile());
    }

    #[test]
    fn an_itemised_cost_that_does_not_add_up_is_flagged() {
        let cost = TransactionCost {
            total_usdc: Decimal::new(25, 0),
            lines: vec![CostLine {
                label: "Liquidity".into(),
                amount_usdc: Decimal::new(20, 0),
                note: None,
            }],
        };
        assert!(!cost.lines_reconcile());
    }

    #[test]
    fn claim_references_round_trip_and_reject_anything_else() {
        let reference = claim_ref("OWNER", "TARGET");
        assert_eq!(reference, "claim:OWNER:TARGET");
        assert_eq!(parse_claim_ref(&reference), Some(("OWNER", "TARGET")));
        for bad in ["OWNER:TARGET", "claim:", "claim:OWNER", "claim::T", "claim:OWNER:", "claim:A:B:C", "create:x", "order:x"] {
            assert_eq!(parse_claim_ref(bad), None, "{bad}");
        }
    }
}
