//! Routing data shapes - request/response DTOs for `services::routing`. Pure data only; the
//! `RoutingStrategy` trait and scoring logic live in `services::routing::service` (with the pure
//! math extracted further into `services::maths::routing`).

use rust_decimal::Decimal;
use serde::Serialize;
use utoipa::ToSchema;

/// One venue's answer for a specific (outcome, size) request, after querying `quote()` and
/// annotating with things `quote()` itself doesn't know about (current health).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RoutingCandidate {
    pub venue_id: &'static str,
    pub venue_market_id: String,
    /// The quoted average fill price.
    #[schema(value_type = String)]
    pub price: Decimal,
    /// How much size this venue could actually fill.
    #[schema(value_type = String)]
    pub executable_size: Decimal,
    /// Whether the venue could fill the *full* requested size at this price - an order-book
    /// venue's quote() already reflects a partial fill if the book was too thin; this flag is
    /// what lets a strategy tell "filled but at a worse price" apart from "not enough liquidity
    /// at all".
    pub fully_executable: bool,
    #[schema(value_type = String)]
    pub fee_usdc: Decimal,
    /// False if this venue is currently unhealthy - see `HealthRegistry`.
    pub routable: bool,
}

/// One candidate plus the numeric score a `RoutingStrategy` gave it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScoredCandidate {
    pub candidate: RoutingCandidate,
    #[schema(value_type = String)]
    pub score: Decimal,
}

/// The router's answer: the top-ranked venue, plus the full comparison.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Recommendation {
    pub venue_id: &'static str,
    pub venue_market_id: String,
    #[schema(value_type = String)]
    pub price: Decimal,
    #[schema(value_type = String)]
    pub executable_size: Decimal,
    #[schema(value_type = String)]
    pub fees: Decimal,
    /// "available" (fills in full) or "partial".
    pub execution: &'static str,
    /// Every candidate considered, ranked - lets a UI show "best overall" while still surfacing
    /// why (what the runner-up venues looked like).
    pub comparison: Vec<ScoredCandidate>,
    /// True when the venues' listings for this question resolve under different rules (overtime,
    /// price source, cutoff). The listings were matched by score, not identity, so the compared
    /// venues are not interchangeable: a client must show this before the user commits money.
    pub rules_differ: bool,
}
