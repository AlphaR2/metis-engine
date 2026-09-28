//! The events engine: turns the venues' raw markets into canonical events, questions, outcomes
//! and listings. Deterministic by design - rules, dictionaries and scoring, no model calls - so
//! every result is reproducible and explainable (docs/aggregator-plan.md, "Matching engine").

pub mod build;
pub mod dictionary;
pub mod extract;
pub mod graph;
pub mod headline;
pub mod matching;
pub mod rating;
pub mod text;
