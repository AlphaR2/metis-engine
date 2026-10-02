//! The venue-side reference to one side of a market, used by the router to quote a listing. The
//! canonical event graph itself lives in `crate::engine::graph`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A venue's native word for a side of a bet
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum VenueSide {
    Yes,
    No,
    Long,
    Short,
}

impl VenueSide {
    /// The wire string a `VenueAdapter::quote()` call expects for this side.
    pub fn as_str(self) -> &'static str {
        match self {
            VenueSide::Yes => "yes",
            VenueSide::No => "no",
            VenueSide::Long => "long",
            VenueSide::Short => "short",
        }
    }
}

/// One venue's copy of a canonical outcome
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VenueMarketRef {
    /// Which venue this ref points into, e.g. "panta".
    pub venue_id: String,
    /// That venue's own id for the market, opaque to us.
    pub venue_market_id: String,
    /// The side that resolves to *this* outcome on *this* venue.
    pub venue_side: VenueSide,
}
