//! The canonical event/proposition/outcome data shapes - the real-world-event anchor that venue
//! markets attach to. event -> proposition -> outcome -> one venue_market per venue backing that
//! outcome. Pure data only; storage/queries live in `repositories::graph_repository`.
//!
//! Terminology: an `Event` is the canonical real-world anchor (e.g. "Man Utd vs Fulham"). What a
//! venue calls a "market" is one `Proposition` (the question, e.g. "who wins?") with its
//! `Outcome`s (the possible answers) - a venue's individual market is represented here as a
//! `VenueMarketRef` attached to each `Outcome`, not as its own top-level type.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::models::venue::Category;

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

/// One possible result of a `Proposition`, e.g. "Yes" or "Man Utd".
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Outcome {
    pub outcome_id: String,
    /// Human-readable name shown in a UI.
    pub label: String,
    /// Every venue currently quoting this outcome.
    pub venue_markets: Vec<VenueMarketRef>,
}

/// A yes/no (or long/short) question about an `Event` - "who wins?". This is what a venue calls
/// its "market" - one proposition, two outcomes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Proposition {
    pub proposition_id: String,
    /// The possible answers - two for every venue integrated so far.
    pub outcomes: Vec<Outcome>,
}

/// A real-world occurrence (a match, an election) that venues take bets on. The canonical
/// anchor: many venues' markets (propositions) can attach to the same event.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Event {
    pub event_id: String,
    pub title: String,
    #[schema(value_type = String)]
    pub category: Category,
    pub start_time: i64,
    pub resolution_time: i64,
    /// Questions raised about this event - currently always exactly one (the binary "who wins?"
    /// every integrated venue asks); the Vec leaves room for a future second, distinct
    /// proposition about the same event (e.g. a total-goals over/under) without changing this
    /// shape.
    pub propositions: Vec<Proposition>,
    /// Set by `matching` when a market couldn't be matched with high confidence to any existing
    /// event and a new one was created as a guess - surfaced so a human/UI can confirm or merge
    /// it rather than the graph silently trusting a low-confidence match forever.
    pub needs_confirmation: bool,
}

/// Whether an event can actually take a bet right now. Every event reaching the graph is already
/// guaranteed non-dead (resolved/cancelled markets are dropped before `attach_market` ever sees
/// them - see `Aggregator::refresh_venue_pages`), so this only distinguishes "open for betting
/// now" from "on the board but hasn't started yet" - both are shown per product direction, just
/// labeled honestly rather than only surfacing what's live this second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventTradeStatus {
    /// `start_time <= now < resolution_time` - live, can be bet on right now.
    Open,
    /// `now < start_time` - on the board, not yet open for betting.
    Upcoming,
}

impl Event {
    /// Computes this event's trade status against `now` - not stored, so it's always current
    /// rather than going stale between refreshes.
    pub fn trade_status(&self, now: i64) -> EventTradeStatus {
        if now < self.start_time { EventTradeStatus::Upcoming } else { EventTradeStatus::Open }
    }
}

/// A trimmed-down `Event`, for the `/events` list view.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EventSummary {
    pub event_id: String,
    pub title: String,
    #[schema(value_type = String)]
    pub category: Category,
    pub start_time: i64,
    pub resolution_time: i64,
    pub needs_confirmation: bool,
    /// How many venue markets back this event, across all outcomes.
    pub venue_count: usize,
    pub trade_status: EventTradeStatus,
}

impl EventSummary {
    /// Builds a summary from `event`, computing `trade_status` against `now`.
    pub fn from_event(event: &Event, now: i64) -> Self {
        EventSummary {
            event_id: event.event_id.clone(),
            title: event.title.clone(),
            category: event.category.clone(),
            start_time: event.start_time,
            resolution_time: event.resolution_time,
            needs_confirmation: event.needs_confirmation,
            venue_count: event
                .propositions
                .iter()
                .flat_map(|p| &p.outcomes)
                .flat_map(|o| &o.venue_markets)
                .count(),
            trade_status: event.trade_status(now),
        }
    }
}
