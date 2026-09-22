//! Market listing and ranking - the `GET /markets` business logic.

pub mod service;

pub use service::{MAX_LIVE_QUOTES, ScoredWith, rank_with_quotes};
