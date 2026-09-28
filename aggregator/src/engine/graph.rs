//! The canonical event graph the `/v1` API serves: Event → Question → Outcome → Listing.
//!
//! Built fresh from the market store on every refresh (`build::build_graph`) and swapped in whole,
//! so it can never accumulate dead markets (audit A7) and reads never wait on a rebuild.

use std::collections::HashMap;

use rust_decimal::Decimal;
use serde::Serialize;
use utoipa::ToSchema;

use super::dictionary::EntityRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    /// Not started; bets open.
    Upcoming,
    /// Started (kickoff passed) and still taking bets.
    Live,
    /// No longer taking bets.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// Who wins a matchup; outcomes are the sides.
    Winner,
    /// One of several mutually exclusive options (candidates, ranges, counts).
    Multi,
    /// A price or level threshold, yes or no.
    Threshold,
    /// Will something happen by a date, yes or no.
    Deadline,
    /// Any other yes/no question.
    Binary,
}

/// One market on one venue, as a way to back an outcome.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Listing {
    pub venue_id: String,
    pub venue_market_id: String,
    pub venue_event_id: Option<String>,
    /// The venue-native side that backs this outcome (`yes` / `no`).
    pub side: String,
    /// Catalog price for that side, 0-1.
    #[schema(value_type = Option<String>)]
    pub price: Option<Decimal>,
    #[schema(value_type = Option<String>)]
    pub volume_usdc: Option<Decimal>,
    pub close_time: i64,
    /// Whether a trade can be routed here. `false` for a listing shown for its price only (a Panta
    /// market past its primary phase); routes and rating skip it.
    pub tradable: bool,
    /// The venue's resolution terms, verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,
    /// 0-100 within this outcome; see `rating`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<super::rating::Rating>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OutcomeNode {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntityRef>,
    /// Every venue listing backing this outcome, best-rated first.
    pub listings: Vec<Listing>,
}

impl OutcomeNode {
    /// The best catalog price across listings.
    pub fn best_price(&self) -> Option<Decimal> {
        self.listings.iter().filter_map(|l| l.price).min()
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QuestionNode {
    /// Stable id (`q_…`) derived from the venue markets it was first built from.
    pub id: String,
    pub kind: QuestionKind,
    /// Short label, e.g. "↑ 90,000" or "Who wins?".
    pub label: String,
    /// The question as a full sentence.
    pub text: String,
    /// The threshold or line, when the question has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub line: Option<Decimal>,
    pub close_time: i64,
    pub outcomes: Vec<OutcomeNode>,
    /// True when the listings merged here come from venues whose rules differ; their prices are
    /// then shown side by side, never ranked as interchangeable.
    pub rules_differ: bool,
}

impl QuestionNode {
    pub fn venue_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> =
            self.outcomes.iter().flat_map(|o| o.listings.iter().map(|l| l.venue_id.as_str())).collect();
        ids.sort();
        ids.dedup();
        ids
    }

    pub fn volume(&self) -> Decimal {
        self.outcomes.iter().flat_map(|o| o.listings.iter()).filter_map(|l| l.volume_usdc).sum()
    }
}

/// Where an event came from on one venue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SourceRef {
    pub venue_id: String,
    pub venue_event_id: String,
    /// The venue's own title for the event, verbatim - often a question.
    pub title: String,
    /// The venue's own category name, verbatim.
    pub venue_category: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EventNode {
    /// Stable id (`evt_…`) derived from the venue event it was built from, or from what its
    /// related events share when several were grouped; a merged-away event's id still resolves
    /// (see `EventGraph::event`).
    pub id: String,
    pub slug: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// What the event covers, in one line: category, how many questions on which venues, and when
    /// they close - "Politics · 3 questions on Jupiter · closes Sep 30, 2026 – Dec 31, 2027".
    pub context: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// When the real-world event starts, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    /// Earliest betting close across its questions.
    pub close_time: i64,
    pub status: EventStatus,
    #[schema(value_type = String)]
    pub volume_usdc: Decimal,
    pub venue_count: usize,
    pub question_count: usize,
    pub sources: Vec<SourceRef>,
    /// 0-1: how much of the metadata was extracted with confidence.
    pub metadata_confidence: f32,
    pub questions: Vec<QuestionNode>,
}

/// The whole graph, with lookups.
#[derive(Debug, Default)]
pub struct EventGraph {
    pub events: Vec<EventNode>,
    by_id: HashMap<String, usize>,
    /// Old event ids that now resolve to a merged event.
    aliases: HashMap<String, String>,
    /// (venue_id, venue_market_id) → (event index, question index).
    listings: HashMap<(String, String), (usize, usize)>,
    pub built_at: i64,
}

impl EventGraph {
    pub fn new(events: Vec<EventNode>, aliases: HashMap<String, String>, built_at: i64) -> Self {
        let by_id = events.iter().enumerate().map(|(i, e)| (e.id.clone(), i)).collect();
        let mut listings = HashMap::new();
        for (e, event) in events.iter().enumerate() {
            for (q, question) in event.questions.iter().enumerate() {
                for listing in question.outcomes.iter().flat_map(|o| &o.listings) {
                    listings.entry((listing.venue_id.clone(), listing.venue_market_id.clone())).or_insert((e, q));
                }
            }
        }
        Self { events, by_id, aliases, listings, built_at }
    }

    /// The event with this id, following an alias left by a merge.
    pub fn event(&self, id: &str) -> Option<&EventNode> {
        let id = self.aliases.get(id).map_or(id, String::as_str);
        self.by_id.get(id).map(|i| &self.events[*i])
    }

    /// The (event, question) holding the listing for this venue market.
    pub fn find_listing(&self, venue_id: &str, venue_market_id: &str) -> Option<(&EventNode, &QuestionNode)> {
        let (e, q) = *self.listings.get(&(venue_id.to_string(), venue_market_id.to_string()))?;
        Some((&self.events[e], &self.events[e].questions[q]))
    }

    /// The question with this id and the event holding it.
    pub fn question(&self, question_id: &str) -> Option<(&EventNode, &QuestionNode)> {
        self.events.iter().find_map(|e| e.questions.iter().find(|q| q.id == question_id).map(|q| (e, q)))
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
        assert_eq!(stable_id("evt", "sports:nfl:atlanta-falcons|green-bay-packers:2026-09-25"), stable_id("evt", "sports:nfl:atlanta-falcons|green-bay-packers:2026-09-25"));
        assert_ne!(stable_id("evt", "a"), stable_id("evt", "b"));
        assert!(stable_id("q", "x").starts_with("q_"));
        // Pinned: an id integrators may have stored must not change between releases.
        assert_eq!(stable_id("evt", "hello"), "evt_a430d84680aabd0b");
    }
}
