//! A wallet's holdings across venues, in one venue-neutral shape.
//!
//! Both venues report positions, but differently: Jupiter gives one record per position with its
//! own pnl and claim flags, Panta gives one row per side with a market phase and no valuation. The
//! adapter normalises each into [`Position`], so the client renders one list and never branches on a
//! venue id. What a venue can't say stays `None` rather than being guessed: an unknown value is
//! shown as unknown, never as zero.

use rust_decimal::Decimal;
use serde::Serialize;
use utoipa::ToSchema;

/// Where a position stands in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PositionState {
    /// The market is still trading or awaiting resolution.
    Open,
    /// The market settled in this position's favour and the payout has not been collected.
    Claimable,
    /// The payout was collected.
    Claimed,
    /// The market settled against this position. Nothing to collect.
    Lost,
    /// The market was cancelled.
    Cancelled,
}

/// One holding: a side of a market owned by the wallet.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Position {
    pub venue_id: String,
    pub venue_market_id: String,
    /// The venue's id for this position where it has one (Jupiter's position account). This is
    /// what `POST /execution/sells` takes.
    pub venue_position_id: Option<String>,
    /// `yes` or `no`.
    pub side: String,
    /// Shares held, as a decimal string.
    #[schema(value_type = String)]
    pub contracts: Decimal,
    /// What the wallet paid per share on average.
    #[schema(value_type = Option<String>)]
    pub avg_price_usdc: Option<Decimal>,
    /// Total spent.
    #[schema(value_type = Option<String>)]
    pub cost_usdc: Option<Decimal>,
    /// What the position would fetch now, when the venue can say (Jupiter's mark to market).
    #[schema(value_type = Option<String>)]
    pub value_usdc: Option<Decimal>,
    /// True when `value_usdc` was worked out from the catalog's last price rather than reported by
    /// the venue. A spot price is not what a sale would fetch, and the catalog is minutes old, so a
    /// client must label it an estimate.
    pub value_is_estimate: bool,
    #[schema(value_type = Option<String>)]
    pub pnl_usdc: Option<Decimal>,
    /// What a claim pays, when known.
    #[schema(value_type = Option<String>)]
    pub payout_usdc: Option<Decimal>,
    pub state: PositionState,
    /// The venue's own phase where it has one (`primary`, `secondary`, `resolved`, `cancelled`).
    pub phase: Option<String>,
    /// Orders placed but not yet filled. Non-zero means the number above may still change.
    pub open_orders: u32,
    /// Pass as `venue_ref` to `POST /execution/claims`. Present only when `state` is `claimable`.
    pub claim_ref: Option<String>,
    /// Whether `POST /execution/sells` can exit this position now.
    pub can_sell: bool,
    /// Market title as the venue reports it, for showing a position the catalog no longer holds.
    pub title: Option<String>,
    /// Filled in by the API from the event graph when the market is in the catalog.
    pub event_id: Option<String>,
    pub event_title: Option<String>,
    /// Unix seconds of the venue's last update to this position, when reported.
    pub updated_at: Option<i64>,
}

/// One venue that could not be read. Its positions are missing from the list, which is not the same
/// as the wallet holding nothing there.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VenueFailure {
    pub venue_id: String,
    pub code: crate::errors::ErrorCode,
    pub message: String,
}

/// `GET /v1/positions`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PositionsResponse {
    pub wallet: String,
    pub positions: Vec<Position>,
    /// Venues that could not be read. **When this is non-empty the list is incomplete**: a client
    /// must say so, and must never present the remaining list as everything the wallet holds.
    pub venues_failed: Vec<VenueFailure>,
    /// Venues that were read successfully, including those where the wallet holds nothing.
    pub venues_ok: Vec<String>,
    /// Unix seconds the venues were read.
    pub fetched_at: i64,
}
