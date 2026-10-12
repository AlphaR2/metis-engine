//! The Settlement part — resolving a market and paying out winners: the
//! Vault account state, the payout/solvency math, and the two instruction
//! handlers (each one holding its own full logic, no separate controller
//! layer).

pub mod backer;
pub mod instructions;
pub mod payout;
pub mod state;
