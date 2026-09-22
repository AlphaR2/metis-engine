//! Cross-venue position data shapes. Pure data only; storage/queries live in
//! `repositories::position_repository`. No wrapping of the underlying venue asset - a position
//! stays wherever it actually lives (Panta/Jupiter/Pascal's own on-chain state); this is purely an
//! identity/data layer so two different products asking about the same wallet see the same
//! canonical position.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// Whether a recorded position is still live or has been closed out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum PositionStatus {
    Open,
    Closed,
}

/// A wallet's holding in one outcome, on one venue - canonical identity only.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Position {
    pub position_id: String,
    pub canonical_market_id: String,
    pub canonical_outcome_id: String,
    pub wallet: String,
    pub venue_id: String,
    /// That venue's own id for this position, opaque to us.
    pub venue_position_id: String,
    #[schema(value_type = String)]
    pub quantity: Decimal,
    #[schema(value_type = String)]
    pub entry_price: Decimal,
    pub status: PositionStatus,
    /// When Metis learned about this position, not when the trade happened.
    pub recorded_at: i64,
}

/// What a caller sends to record a position that already happened elsewhere.
#[derive(Debug, Deserialize, ToSchema)]
pub struct RecordPositionRequest {
    pub canonical_market_id: String,
    pub canonical_outcome_id: String,
    pub wallet: String,
    pub venue_id: String,
    pub venue_position_id: String,
    #[schema(value_type = String)]
    pub quantity: Decimal,
    #[schema(value_type = String)]
    pub entry_price: Decimal,
}

/// Query params for `GET /positions` - which wallet to look up.
#[derive(Debug, Deserialize, IntoParams)]
pub struct ListPositionsQuery {
    pub wallet: String,
}
