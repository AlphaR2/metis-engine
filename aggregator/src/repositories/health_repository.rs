//! In-memory store for per-venue health - MVP scope per plan, no persistence. A venue isn't good
//! merely because its last known quote looked good - the router needs to know whether that venue
//! is actually reachable right now before trusting it.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use utoipa::ToSchema;

use crate::errors::AdapterError;
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
    /// Unix timestamp of the most recent failure that counted toward the streak.
    pub last_failed_call: Option<i64>,
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
            last_failed_call: None,
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
        entry.last_failed_call = Some(now_unix());
        entry.api_available = entry.consecutive_failures < MAX_CONSECUTIVE_FAILURES;
    }

    /// Records a failed call according to what the error means:
    ///
    /// - a *business* outcome (no liquidity, unknown market) is the venue answering correctly and
    ///   is not recorded at all;
    /// - a 429 is backpressure, not an outage: noted in `last_error` but it never advances the
    ///   failure streak, so a burst of traffic can't take a working venue out of routing;
    /// - anything else counts toward the streak.
    pub fn record_error(&self, venue_id: &'static str, error: &AdapterError) {
        if error.is_business_outcome() {
            return;
        }
        if error.is_rate_limited() {
            crate::metrics::VENUE_ERRORS.with_label_values(&[venue_id, "rate_limited"]).inc();
            let mut state = self.state.lock().expect("health registry lock poisoned");
            let entry = state.entry(venue_id).or_insert_with(|| VenueHealth::new(venue_id));
            entry.last_error = Some(format!("rate limited: {error}"));
            return;
        }
        crate::metrics::VENUE_ERRORS.with_label_values(&[venue_id, "error"]).inc();
        self.record_failure(venue_id, error.to_string());
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

    /// Circuit breaker for the refresh: true while a venue that has failed too many calls in a row
    /// is still inside `cooldown` of its last failure. The refresh skips it (keeping its last good
    /// catalog) instead of spending a full pass of timeouts on it; after the cooldown one pass is
    /// let through, and a success closes the breaker.
    pub fn breaker_open(&self, venue_id: &str, now: i64, cooldown: std::time::Duration) -> bool {
        let state = self.state.lock().expect("health registry lock poisoned");
        state.get(venue_id).is_some_and(|h| {
            !h.is_routable() && h.last_failed_call.is_some_and(|at| now - at < cooldown.as_secs() as i64)
        })
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
    fn rate_limiting_never_takes_a_venue_out_of_routing() {
        let registry = HealthRegistry::new(["jupiter"].into_iter());
        for _ in 0..10 {
            registry.record_error("jupiter", &AdapterError::Venue("jupiter returned 429 Too Many Requests".to_string()));
        }
        assert!(registry.is_routable("jupiter"));
        assert!(registry.snapshot()[0].last_error.as_deref().unwrap().starts_with("rate limited"));
    }

    #[test]
    fn business_outcomes_are_not_recorded() {
        let registry = HealthRegistry::new(["panta"].into_iter());
        for _ in 0..10 {
            registry.record_error("panta", &AdapterError::MarketNotFound("m".to_string()));
        }
        assert!(registry.is_routable("panta"));
        assert!(registry.snapshot()[0].last_error.is_none());
    }

    #[test]
    fn real_errors_still_count() {
        let registry = HealthRegistry::new(["panta"].into_iter());
        for _ in 0..MAX_CONSECUTIVE_FAILURES {
            registry.record_error("panta", &AdapterError::Venue("503 Service Unavailable".to_string()));
        }
        assert!(!registry.is_routable("panta"));
    }

    #[test]
    fn the_breaker_opens_after_repeated_failures_and_closes_after_the_cooldown() {
        let registry = HealthRegistry::new(["panta"].into_iter());
        let cooldown = std::time::Duration::from_secs(300);
        registry.record_failure("panta", "timeout".to_string());
        assert!(!registry.breaker_open("panta", now_unix(), cooldown), "one failure is not enough");
        for _ in 1..MAX_CONSECUTIVE_FAILURES {
            registry.record_failure("panta", "timeout".to_string());
        }
        assert!(registry.breaker_open("panta", now_unix(), cooldown));
        assert!(!registry.breaker_open("panta", now_unix() + 301, cooldown), "half-open after the cooldown");
        registry.record_success("panta");
        assert!(!registry.breaker_open("panta", now_unix(), cooldown));
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
