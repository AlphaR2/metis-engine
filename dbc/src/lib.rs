//! # Open Pool: Metis' use of Meteora's Dynamic Bonding Curve (DBC)
//!
//! Read `dbc/README.md` first: it explains the flow and lists every file that takes part.
//!
//! This crate holds what every part of Metis that touches the Open Pool has to agree on, and nothing else:
//!
//! - [`args`]: the exact byte layout of the `initialize_backed_market` instruction data;
//! - [`backer`]: a backer's share arithmetic and the escrow-account address.
//!
//! The on-chain program (`programs/core`), the aggregator and the operator tools all import from here, so the
//! instruction layout and the redemption maths are written once. The parts that need the program's own state
//! (the instruction handlers) live in `programs/core/src/dbc`.

pub mod args;
pub mod backer;

pub use args::InitializeBackedMarketArgs;
pub use backer::{ata_address, pro_rata};

/// A Meteora raise returns about this share of the USDC raised, unlocked, right after migration: 89% of the
/// liquidity is unlocked and 11% vests behind a cliff (Meteora requires at least 10% to stay locked). A raise
/// is therefore sized `seed / UNLOCKED_FACTOR` so the unlocked part covers the seed. Measured 0.88822 on the
/// real programs (`scripts/dbc-spike`); the keeper uses the same figure.
pub const UNLOCKED_FACTOR: f64 = 0.888;
