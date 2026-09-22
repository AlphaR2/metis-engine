//! In-memory store for the canonical event graph - MVP scope per plan, no persistence.

use std::collections::HashMap;

use crate::models::event::{Event, VenueMarketRef};
use crate::models::venue::Category;

/// The whole set of canonical events known so far - the aggregator's shared state.
#[derive(Debug, Default)]
pub struct CanonicalGraph {
    events: HashMap<String, Event>,
    /// Counter used to hand out fresh, unique event ids.
    next_event_seq: u64,
}

impl CanonicalGraph {
    /// An empty graph, before anything has been discovered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every known event, in no particular order.
    pub fn events(&self) -> impl Iterator<Item = &Event> {
        self.events.values()
    }

    /// Events worth showing right now: starting within `within_hours` of `now`, or already
    /// started but not yet resolved (so a live event doesn't vanish from the list the moment it
    /// kicks off), optionally narrowed to one `category`. Sorted soonest-first by `start_time` -
    /// unlike `events()`, order here is the whole point of the query. Filtering lives here, at
    /// the canonical-event level, rather than being pushed down into each venue's own query
    /// params - venues don't share one category taxonomy, so filtering after matching/grouping
    /// is the only place it means the same thing for every event regardless of which venues back
    /// it. `now` and `within_hours` are taken as plain params rather than read internally, so
    /// this stays a pure, easily-tested function; the caller (the API route) owns clamping
    /// `within_hours` to a sane range and supplying the real clock.
    pub fn events_within(&self, now: i64, within_hours: i64, category: Option<&Category>) -> Vec<&Event> {
        let horizon = now + within_hours * 3600;
        let mut events: Vec<&Event> = self
            .events
            .values()
            .filter(|event| event.start_time <= horizon && event.resolution_time >= now)
            .filter(|event| category.is_none_or(|c| &event.category == c))
            .collect();
        events.sort_by_key(|event| event.start_time);
        events
    }

    /// Looks up one event by id.
    pub fn get(&self, event_id: &str) -> Option<&Event> {
        self.events.get(event_id)
    }

    /// Looks up one event by id, mutably.
    pub fn get_mut(&mut self, event_id: &str) -> Option<&mut Event> {
        self.events.get_mut(event_id)
    }

    /// Adds or replaces an event, keyed by its own id.
    pub fn insert(&mut self, event: Event) {
        self.events.insert(event.event_id.clone(), event);
    }

    /// Every venue market currently attached anywhere under `event_id`/`outcome_id`, used by the
    /// router to know which adapters to actually query for a quote.
    pub fn venue_markets_for_outcome(
        &self,
        event_id: &str,
        outcome_id: &str,
    ) -> Vec<VenueMarketRef> {
        self.events
            .get(event_id)
            .into_iter()
            .flat_map(|event| &event.propositions)
            .flat_map(|prop| &prop.outcomes)
            .filter(|outcome| outcome.outcome_id == outcome_id)
            .flat_map(|outcome| outcome.venue_markets.clone())
            .collect()
    }

    /// Hands out the next unused event id, e.g. "evt_3".
    pub fn next_event_id(&mut self) -> String {
        self.next_event_seq += 1;
        format!("evt_{}", self.next_event_seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::venue::Category;

    fn event(id: &str, start_time: i64, resolution_time: i64) -> Event {
        Event {
            event_id: id.to_string(),
            title: id.to_string(),
            category: Category::Sports,
            start_time,
            resolution_time,
            propositions: vec![],
            needs_confirmation: false,
        }
    }

    const NOW: i64 = 1_000_000;
    const HOUR: i64 = 3600;

    #[test]
    fn includes_an_event_starting_within_the_window() {
        let mut graph = CanonicalGraph::new();
        graph.insert(event("soon", NOW + 10 * HOUR, NOW + 20 * HOUR));
        let events = graph.events_within(NOW, 72, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_id, "soon");
    }

    #[test]
    fn excludes_an_event_starting_after_the_window() {
        let mut graph = CanonicalGraph::new();
        graph.insert(event("far", NOW + 100 * HOUR, NOW + 200 * HOUR));
        assert!(graph.events_within(NOW, 72, None).is_empty());
    }

    #[test]
    fn includes_an_already_started_event_that_has_not_resolved_yet() {
        let mut graph = CanonicalGraph::new();
        graph.insert(event("live", NOW - 10 * HOUR, NOW + 2 * HOUR));
        let events = graph.events_within(NOW, 72, None);
        assert_eq!(events.len(), 1, "a live, unresolved event should still show up");
    }

    #[test]
    fn excludes_an_event_that_has_already_resolved() {
        let mut graph = CanonicalGraph::new();
        graph.insert(event("done", NOW - 20 * HOUR, NOW - HOUR));
        assert!(graph.events_within(NOW, 72, None).is_empty());
    }

    #[test]
    fn sorts_soonest_first() {
        let mut graph = CanonicalGraph::new();
        graph.insert(event("later", NOW + 50 * HOUR, NOW + 60 * HOUR));
        graph.insert(event("soonest", NOW + 5 * HOUR, NOW + 10 * HOUR));
        graph.insert(event("middle", NOW + 20 * HOUR, NOW + 30 * HOUR));

        let events = graph.events_within(NOW, 72, None);
        let ids: Vec<&str> = events.iter().map(|e| e.event_id.as_str()).collect();
        assert_eq!(ids, vec!["soonest", "middle", "later"]);
    }

    #[test]
    fn category_filter_narrows_to_matching_events_only() {
        let mut graph = CanonicalGraph::new();
        graph.insert(event("sporty", NOW + 5 * HOUR, NOW + 10 * HOUR));
        let mut political = event("polity", NOW + 6 * HOUR, NOW + 10 * HOUR);
        political.category = Category::Politics;
        graph.insert(political);

        let events = graph.events_within(NOW, 72, Some(&Category::Politics));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_id, "polity");
    }
}
