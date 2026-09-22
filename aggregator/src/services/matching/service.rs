//! Basic automated event matching: given a venue's raw `VenueMarket`, decide which canonical
//! `Event` it belongs to (by title similarity + resolution_time closeness), or create a new
//! provisional one if nothing matches well enough. The title-similarity math itself lives in
//! `services::maths::matching`; this module owns the graph-querying orchestration. Known gap:
//! doesn't resolve genuine abbreviations ("Man Utd" vs "Manchester United") - those fall through
//! to a new `needs_confirmation` event for a human (or a smarter future matcher) to merge.

use crate::models::event::{Event, Outcome, Proposition, VenueMarketRef, VenueSide};
use crate::models::venue::{OutcomeFraming, VenueMarket};
use crate::repositories::graph_repository::CanonicalGraph;
use crate::services::maths::matching::titles_match;

/// How close two resolution times can be and still be considered "the same event" - loose enough
/// to absorb small per-venue clock/rounding differences, tight enough not to conflate distinct
/// events in the same category resolving on the same day.
const RESOLUTION_TIME_TOLERANCE_SECS: i64 = 3600;

/// Finds an existing event this market plausibly belongs to, by title + time.
fn find_matching_event(graph: &CanonicalGraph, market: &VenueMarket) -> Option<String> {
    graph
        .events()
        .find(|event| {
            (event.resolution_time - market.resolution_time).abs() <= RESOLUTION_TIME_TOLERANCE_SECS
                && titles_match(&event.title, &market.title)
        })
        .map(|event| event.event_id.clone())
}

/// The venue-native sides for each of a binary proposition's two outcomes, and the labels
/// a UI should show - framing-dependent since "Yes"/"No" isn't meaningful for a LongShort venue
/// like Pascal, and "long"/"short" isn't a real side on a YesNo venue like Panta/Jupiter.
fn outcome_sides_and_labels(framing: OutcomeFraming) -> ((VenueSide, VenueSide), (&'static str, &'static str)) {
    match framing {
        OutcomeFraming::YesNo => ((VenueSide::Yes, VenueSide::No), ("Yes", "No")),
        OutcomeFraming::LongShort => ((VenueSide::Long, VenueSide::Short), ("Long", "Short")),
    }
}

/// A binary market maps to one proposition with two outcomes - the common case for every venue
/// integrated so far. Labels are picked from whichever venue created this event first; a second
/// venue with different framing still attaches fine (its own `venue_side` is recorded per-ref),
/// it just doesn't get to rename the canonical labels.
fn binary_proposition(event_id: &str, framing: OutcomeFraming) -> Proposition {
    let (_, (primary_label, secondary_label)) = outcome_sides_and_labels(framing);
    Proposition {
        proposition_id: format!("{event_id}_prop_1"),
        outcomes: vec![
            Outcome {
                outcome_id: format!("{event_id}_primary"),
                label: primary_label.to_string(),
                venue_markets: vec![],
            },
            Outcome {
                outcome_id: format!("{event_id}_secondary"),
                label: secondary_label.to_string(),
                venue_markets: vec![],
            },
        ],
    }
}

/// Records `market` against both of `event`'s outcomes, unless already attached.
fn attach_venue_market_ref(
    event: &mut Event,
    venue_id: &'static str,
    market: &VenueMarket,
    framing: OutcomeFraming,
) {
    let already_attached = event
        .propositions
        .iter()
        .flat_map(|p| &p.outcomes)
        .flat_map(|o| &o.venue_markets)
        .any(|r| r.venue_id == venue_id && r.venue_market_id == market.venue_market_id);
    if already_attached {
        return;
    }

    let ((primary_side, secondary_side), _) = outcome_sides_and_labels(framing);

    // Same market/symbol backs both outcomes on every venue integrated so far (Panta and
    // Jupiter's yes/no, Pascal's long/short) - it's one order book/curve quoted with a different
    // side per outcome, not two separate venue markets. Attach a ref to each outcome, carrying
    // the side that actually resolves to it on this venue.
    if let Some(prop) = event.propositions.first_mut()
        && let [primary, secondary] = &mut prop.outcomes[..] {
            primary.venue_markets.push(VenueMarketRef {
                venue_id: venue_id.to_string(),
                venue_market_id: market.venue_market_id.clone(),
                venue_side: primary_side,
            });
            secondary.venue_markets.push(VenueMarketRef {
                venue_id: venue_id.to_string(),
                venue_market_id: market.venue_market_id.clone(),
                venue_side: secondary_side,
            });
        }
}

/// Attaches `market` (from `venue_id`, framed as `framing`) into `graph`: finds an existing
/// matching event or creates a new provisional one, then records this venue's market_id against
/// both of that event's outcomes (with the correct venue-native side per outcome).
pub fn attach_market(
    graph: &mut CanonicalGraph,
    venue_id: &'static str,
    framing: OutcomeFraming,
    market: VenueMarket,
) {
    let event_id = match find_matching_event(graph, &market) {
        Some(id) => id,
        None => {
            let event_id = graph.next_event_id();
            graph.insert(Event {
                event_id: event_id.clone(),
                title: market.title.clone(),
                category: market.category.clone(),
                start_time: market.start_time,
                resolution_time: market.resolution_time,
                propositions: vec![binary_proposition(&event_id, framing)],
                // A brand new event created from a single venue's market is a guess by
                // construction until a second venue corroborates it, or a human confirms it.
                needs_confirmation: true,
            });
            event_id
        }
    };

    if let Some(event) = graph.get_mut(&event_id) {
        attach_venue_market_ref(event, venue_id, &market, framing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::venue::Category;

    /// A minimal fake venue market for tests - title/time are what matching cares about.
    fn market(title: &str, resolution_time: i64) -> VenueMarket {
        VenueMarket {
            venue_id: "panta".to_string(),
            venue_market_id: "m1".to_string(),
            title: title.to_string(),
            event: None,
            rules: None,
            category: Category::Sports,
            phase: "open".to_string(),
            start_time: 1_000,
            close_time: resolution_time,
            resolution_time,
            yes_price: Some("0.5".to_string()),
            no_price: Some("0.5".to_string()),
            liquidity: None,
            dead: false,
        }
    }

    #[test]
    fn first_market_creates_a_new_provisional_event() {
        let mut graph = CanonicalGraph::new();
        attach_market(&mut graph, "panta", OutcomeFraming::YesNo, market("Man Utd vs Fulham", 5_000));

        let events: Vec<_> = graph.events().collect();
        assert_eq!(events.len(), 1);
        assert!(events[0].needs_confirmation);
        assert_eq!(events[0].propositions[0].outcomes[0].venue_markets.len(), 1);
        assert_eq!(events[0].propositions[0].outcomes[0].venue_markets[0].venue_side, VenueSide::Yes);
        assert_eq!(events[0].propositions[0].outcomes[1].venue_markets[0].venue_side, VenueSide::No);
    }

    #[test]
    fn near_identical_title_and_close_time_attaches_to_the_same_event() {
        let mut graph = CanonicalGraph::new();
        attach_market(&mut graph, "panta", OutcomeFraming::YesNo, market("Man Utd vs Fulham", 5_000));
        attach_market(&mut graph, "jupiter", OutcomeFraming::YesNo, market("man utd vs fulham", 5_100));

        let events: Vec<_> = graph.events().collect();
        assert_eq!(events.len(), 1, "should have merged into one event, got: {events:?}");
        assert_eq!(events[0].propositions[0].outcomes[0].venue_markets.len(), 2);
    }

    #[test]
    fn a_long_short_venue_attaches_with_its_own_native_sides() {
        let mut graph = CanonicalGraph::new();
        attach_market(&mut graph, "panta", OutcomeFraming::YesNo, market("Lakers vs Celtics", 5_000));
        attach_market(&mut graph, "pascal", OutcomeFraming::LongShort, market("Lakers vs Celtics", 5_050));

        let events: Vec<_> = graph.events().collect();
        assert_eq!(events.len(), 1);
        let primary_refs = &events[0].propositions[0].outcomes[0].venue_markets;
        let pascal_ref = primary_refs.iter().find(|r| r.venue_id == "pascal").unwrap();
        assert_eq!(pascal_ref.venue_side, VenueSide::Long);
        let secondary_refs = &events[0].propositions[0].outcomes[1].venue_markets;
        let pascal_secondary = secondary_refs.iter().find(|r| r.venue_id == "pascal").unwrap();
        assert_eq!(pascal_secondary.venue_side, VenueSide::Short);
    }

    #[test]
    fn genuine_abbreviation_mismatch_is_left_for_manual_confirmation() {
        let mut graph = CanonicalGraph::new();
        attach_market(&mut graph, "panta", OutcomeFraming::YesNo, market("Man Utd vs Fulham", 5_000));
        attach_market(&mut graph, "jupiter", OutcomeFraming::YesNo, market("Manchester United vs Fulham", 5_100));

        // Not merged - both sit as separate, needs_confirmation events until a human (or a
        // future smarter matcher) links them.
        let events: Vec<_> = graph.events().collect();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.needs_confirmation));
    }

    #[test]
    fn distinct_title_creates_a_separate_event() {
        let mut graph = CanonicalGraph::new();
        attach_market(&mut graph, "panta", OutcomeFraming::YesNo, market("Man Utd vs Fulham", 5_000));
        attach_market(&mut graph, "jupiter", OutcomeFraming::YesNo, market("Lakers vs Celtics", 5_000));

        assert_eq!(graph.events().count(), 2);
    }

    #[test]
    fn far_apart_resolution_times_keep_events_separate_even_with_identical_titles() {
        let mut graph = CanonicalGraph::new();
        attach_market(&mut graph, "panta", OutcomeFraming::YesNo, market("Title Match", 0));
        attach_market(&mut graph, "jupiter", OutcomeFraming::YesNo, market("Title Match", 100_000));

        assert_eq!(graph.events().count(), 2);
    }
}
