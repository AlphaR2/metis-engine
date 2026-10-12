//! The Metis venue: this project's own prediction-market program, read straight from chain.
//!
//! Unlike Panta and Jupiter there is no HTTP API behind it - the program's accounts are the
//! catalog - so it has a `chain.rs` (JSON-RPC), a `view.rs` (accounts into the aggregator's
//! shapes, pure and tested) and `orders.rs` (unsigned buy and claim transactions).
//!
//! Configuration, all optional except the RPC node you point it at:
//!   METIS_RPC_URL          Solana RPC endpoint (default: devnet)
//!   METIS_PROGRAM_ID       the deployed core program (default: this build's program id)
//!   METIS_CONFIG           the Config account (default: derived from this build's trust roots -
//!                          build with `--features devnet` / `mainnet` to match a deployment)
//!   METIS_REFERENCE_USDC   the bet size the catalog's displayed prices are quoted for (default 10)

mod chain;
mod create;
mod dbc;
mod orders;
mod secondary;
mod service;
pub mod view;

pub use service::{MetisAdapter, MetisSettings};

#[cfg(test)]
mod tests;
