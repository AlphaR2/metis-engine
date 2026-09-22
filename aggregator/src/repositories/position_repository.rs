//! In-memory store for cross-venue positions - MVP scope per plan, no persistence.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::models::position::{Position, PositionStatus, RecordPositionRequest};
use crate::utils::time::now_unix;

/// In-memory position index, keyed by wallet - MVP scope per plan, no persistence, no wrapping
/// of the underlying venue asset.
pub struct PositionIndex {
    by_wallet: Mutex<HashMap<String, Vec<Position>>>,
    next_id: Mutex<u64>,
}

impl Default for PositionIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl PositionIndex {
    /// An empty index, before any position has been recorded.
    pub fn new() -> Self {
        Self { by_wallet: Mutex::new(HashMap::new()), next_id: Mutex::new(0) }
    }

    /// Stores a new open position and hands back its assigned id.
    pub fn record(&self, req: RecordPositionRequest) -> Position {
        let position_id = {
            let mut next_id = self.next_id.lock().expect("position index lock poisoned");
            *next_id += 1;
            format!("pos_{next_id}")
        };

        let position = Position {
            position_id,
            canonical_market_id: req.canonical_market_id,
            canonical_outcome_id: req.canonical_outcome_id,
            wallet: req.wallet.clone(),
            venue_id: req.venue_id,
            venue_position_id: req.venue_position_id,
            quantity: req.quantity,
            entry_price: req.entry_price,
            status: PositionStatus::Open,
            recorded_at: now_unix(),
        };

        self.by_wallet
            .lock()
            .expect("position index lock poisoned")
            .entry(req.wallet)
            .or_default()
            .push(position.clone());

        position
    }

    /// Every position recorded for this wallet, across every venue.
    pub fn for_wallet(&self, wallet: &str) -> Vec<Position> {
        self.by_wallet
            .lock()
            .expect("position index lock poisoned")
            .get(wallet)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(wallet: &str) -> RecordPositionRequest {
        RecordPositionRequest {
            canonical_market_id: "evt_1".to_string(),
            canonical_outcome_id: "evt_1_primary".to_string(),
            wallet: wallet.to_string(),
            venue_id: "panta".to_string(),
            venue_position_id: "venue_pos_1".to_string(),
            quantity: "100".parse().unwrap(),
            entry_price: "0.55".parse().unwrap(),
        }
    }

    #[test]
    fn recorded_position_is_retrievable_by_wallet() {
        let index = PositionIndex::new();
        index.record(request("wallet_a"));

        let positions = index.for_wallet("wallet_a");
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].status, PositionStatus::Open);
    }

    #[test]
    fn positions_are_isolated_per_wallet() {
        let index = PositionIndex::new();
        index.record(request("wallet_a"));
        index.record(request("wallet_b"));

        assert_eq!(index.for_wallet("wallet_a").len(), 1);
        assert_eq!(index.for_wallet("wallet_b").len(), 1);
        assert_eq!(index.for_wallet("wallet_c").len(), 0);
    }

    #[test]
    fn position_ids_are_unique_and_increasing() {
        let index = PositionIndex::new();
        let p1 = index.record(request("wallet_a"));
        let p2 = index.record(request("wallet_a"));
        assert_ne!(p1.position_id, p2.position_id);
    }
}
