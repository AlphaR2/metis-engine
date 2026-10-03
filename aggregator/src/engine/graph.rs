//! The event graph the `/v1` API serves: Event → Markets.
//!
//! An **event** is our own aggregation of venue markets about one real-world subject. It carries our
//! metadata (title, image, category, dates, volume, rating) and *why* its markets were grouped. Its
//! **markets** are the venues' own markets, exactly as they were sent, each annotated (never
//! rewritten) with our rating and the identical questions other venues list.
//!
//! Built fresh from the market store on every refresh (`events::build_events`) and swapped in whole,
//! so it can never accumulate dead markets and reads never wait on a rebuild.

use std::collections::HashMap;

use rust_decimal::Decimal;
use serde::Serialize;
use utoipa::ToSchema;

use super::dictionary::EntityRef;
use super::rating::Rating;
use crate::models::venue::VenueMarket;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    /// Not started; bets open.
    Upcoming,
    /// Started and still taking bets.
    Live,
    /// No longer taking bets.
    Closed,
}

/// A pointer to one venue market.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MarketRef {
    pub venue_id: String,
    pub venue_market_id: String,
}

/// One venue market inside an event: the market untouched, plus our annotations.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MarketNode {
    /// The venue's market exactly as it was sent: title, outcomes, prices, volume, rules, image.
    pub market: VenueMarket,
    /// Our 0-100 rating among this event's markets, with its breakdown.
    pub rating: Rating,
    /// The identical question on another venue, so the two can be read side by side.
    pub same_question_as: Vec<MarketRef>,
}

impl MarketNode {
    pub fn volume(&self) -> Decimal {
        self.market.liquidity.unwrap_or(Decimal::ZERO)
    }
}

/// Why an event's markets were grouped together.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Grouping {
    /// `venue_event` (the venue grouped them), `match` (the same two sides on the same day), `price`
    /// (one asset in one month), `terms` (the same distinctive words), `ai` (a model decided) or
    /// `single` (nothing else matched, so the market is its own event).
    pub reason: String,
    /// A sentence saying what they share.
    pub detail: String,
    /// 0-1.
    pub confidence: f32,
}

/// Where an event's markets came from on one venue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SourceRef {
    pub venue_id: String,
    pub venue_event_id: String,
    /// The venue's own title for the event, verbatim.
    pub title: String,
    /// The venue's own category name, verbatim.
    pub venue_category: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EventNode {
    /// Stable id (`evt_…`) while the event's markets are stable.
    pub id: String,
    pub slug: String,
    /// A headline naming the subject, never a question.
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Shared category name (see `CategoryAliases`).
    pub category: String,
    /// League or asset class, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subcategory: Option<String>,
    /// What the event is about, by extraction: sports, crypto, finance, …
    pub domain: String,
    pub tags: Vec<String>,
    pub entities: Vec<EntityRef>,
    /// The best member's image. Absent when no venue sent one; clients draw a category fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// When the real-world event starts, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    /// Earliest betting close across its markets.
    pub close_time: i64,
    pub status: EventStatus,
    #[schema(value_type = String)]
    pub volume_usdc: Decimal,
    pub venues: Vec<String>,
    pub venue_count: usize,
    pub market_count: usize,
    /// Our 0-100 rating of the event itself: activity, venue coverage and depth.
    pub rating: Rating,
    pub grouping: Grouping,
    pub sources: Vec<SourceRef>,
    /// The raw venue markets, best-rated first.
    pub markets: Vec<MarketNode>,
}

/// The whole graph, with lookups.
#[derive(Debug, Default)]
pub struct EventGraph {
    pub events: Vec<EventNode>,
    by_id: HashMap<String, usize>,
    /// (venue_id, venue_market_id) → (event index, market index).
    markets: HashMap<(String, String), (usize, usize)>,
    pub built_at: i64,
}

impl EventGraph {
    pub fn new(events: Vec<EventNode>, built_at: i64) -> Self {
        let by_id = events.iter().enumerate().map(|(i, e)| (e.id.clone(), i)).collect();
        let mut markets = HashMap::new();
        for (e, event) in events.iter().enumerate() {
            for (m, node) in event.markets.iter().enumerate() {
                markets.entry((node.market.venue_id.clone(), node.market.venue_market_id.clone())).or_insert((e, m));
            }
        }
        Self { events, by_id, markets, built_at }
    }

    pub fn event(&self, id: &str) -> Option<&EventNode> {
        self.by_id.get(id).map(|i| &self.events[*i])
    }

    /// The (event, market) holding this venue market.
    pub fn find_market(&self, venue_id: &str, venue_market_id: &str) -> Option<(&EventNode, &MarketNode)> {
        let (e, m) = *self.markets.get(&(venue_id.to_string(), venue_market_id.to_string()))?;
        Some((&self.events[e], &self.events[e].markets[m]))
    }
}

/// A short, stable id: `prefix` + the first 16 hex digits of a 64-bit FNV-1a hash of `key`.
/// FNV rather than `DefaultHasher` because it is specified, so ids survive toolchain upgrades -
/// integrators store these.
pub fn stable_id(prefix: &str, key: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{prefix}_{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_ids_are_deterministic_and_prefixed() {
        assert_eq!(stable_id("evt", "a|b:2026-09-25"), stable_id("evt", "a|b:2026-09-25"));
        assert_ne!(stable_id("evt", "a"), stable_id("evt", "b"));
        assert!(stable_id("evt", "x").starts_with("evt_"));
        // Pinned: an id integrators may have stored must not change between releases.
        assert_eq!(stable_id("evt", "hello"), "evt_a430d84680aabd0b");
    }
}
