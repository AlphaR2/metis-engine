//! The events engine: turns the venues' raw markets into events, our own aggregation of markets
//! about one real-world subject, each carrying its markets exactly as the venues sent them.
//!
//! Rules decide almost everything and are deterministic (`subject`, `cluster`, `events`); a model
//! may assist with what the rules leave alone (`ai`), never overriding a hard conflict.

pub mod ai;
pub mod cluster;
pub mod dictionary;
pub mod events;
pub mod extract;
pub mod graph;
pub mod headline;
pub mod rating;
pub mod subject;
pub mod text;
