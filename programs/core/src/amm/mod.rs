//! The AMM part — everything about primary-market pricing and trading:
//! the account state, the curve math, and the two instruction handlers
//! (each one holding its own full logic, no separate controller layer).

pub mod condition;
pub mod curve;
pub mod metadata;
pub mod quote;
pub mod instructions;
pub mod state;
