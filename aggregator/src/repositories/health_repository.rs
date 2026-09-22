//! In-memory store for per-venue health - MVP scope per plan, no persistence. A venue isn't good
//! merely because its last known quote looked good - the router needs to know whether that venue
//! is actually reachable right now before trusting it.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use utoipa::ToSchema;

use crate::utils::time::now_unix;

/// A venue gets excluded from routing once it has failed this many calls in a row - an
/// occasional blip shouldn't drop a venue, but a venue that's actually down should stop being
/// recommended even if its last successful quote is still cached somewhere.
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// Live reachability snapshot for one venue.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VenueHealth {
    pub venue_id: &'static str,
    pub api_available: bool,
    /// Unix timestamp of the last call that worked.
    pub last_successful_call: Option<i64>,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
}

impl VenueHealth {
    /// A brand-new, assumed-healthy record for a venue.
    fn new(venue_id: &'static str) -> Self {
        Self {
            venue_id,
            api_available: true,
            last_successful_call: None,
            last_error: None,
            consecutive_failures: 0,
        }
    }

    /// Safe to route to right now.
    pub fn is_routable(&self) -> bool {
        self.consecutive_failures < MAX_CONSECUTIVE_FAILURES
    }
}

/// In-memory health state for every registered venue - MVP scope per plan, no persistence.
pub struct HealthRegistry {
    state: Mutex<HashMap<&'static str, VenueHealth>>,
}

impl HealthRegistry {
    /// Starts every given venue out as healthy.
    pub fn new(venue_ids: impl Iterator<Item = &'static str>) -> Self {
        let state = venue_ids.map(|id| (id, VenueHealth::new(id))).collect();
        Self { state: Mutex::new(state) }
    }

    /// Marks a call as successful, resetting the failure streak.
    pub fn record_success(&self, venue_id: &'static str) {
        let mut state = self.state.lock().expect("health registry lock poisoned");
        let entry = state.entry(venue_id).or_insert_with(|| VenueHealth::new(venue_id));
        entry.api_available = true;
        entry.last_successful_call = Some(now_unix());
        entry.last_error = None;
        entry.consecutive_failures = 0;
    }

    /// Marks a call as failed, bumping the failure streak.
    pub fn record_failure(&self, venue_id: &'static str, error: String) {
        let mut state = self.state.lock().expect("health registry lock poisoned");
        let entry = state.entry(venue_id).or_insert_with(|| VenueHealth::new(venue_id));
        entry.consecutive_failures += 1;
        entry.last_error = Some(error);
        entry.api_available = entry.consecutive_failures < MAX_CONSECUTIVE_FAILURES;
    }

    /// True unless this venue has failed too many calls in a row.
    pub fn is_routable(&self, venue_id: &str) -> bool {
        self.state
            .lock()
            .expect("health registry lock poisoned")
            .get(venue_id)
            .map(|h| h.is_routable())
            .unwrap_or(true)
    }

    /// Every venue's current health, for the `/venues/health` endpoint.
    pub fn snapshot(&self) -> Vec<VenueHealth> {
        self.state.lock().expect("health registry lock poisoned").values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_routable_and_stays_routable_after_success() {
        let registry = HealthRegistry::new(["panta"].into_iter());
        registry.record_success("panta");
        assert!(registry.is_routable("panta"));
    }

    #[test]
    fn excludes_venue_after_enough_consecutive_failures() {
        let registry = HealthRegistry::new(["panta"].into_iter());
        for _ in 0..MAX_CONSECUTIVE_FAILURES {
            registry.record_failure("panta", "timeout".to_string());
        }
        assert!(!registry.is_routable("panta"));
    }

    #[test]
    fn a_success_resets_the_failure_streak() {
        let registry = HealthRegistry::new(["panta"].into_iter());
        registry.record_failure("panta", "timeout".to_string());
        registry.record_failure("panta", "timeout".to_string());
        registry.record_success("panta");
        assert!(registry.is_routable("panta"));
    }
}
