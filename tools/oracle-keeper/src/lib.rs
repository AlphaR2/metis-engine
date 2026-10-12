//! Reads match results from sports-data providers and reports them to the Metis oracle.
//!
//! The pieces, in the order data flows through them:
//!   `sources`  - a provider's response, translated into an `Observation`
//!   `planner`  - what claim to send next, given the chain and the observation
//!   `keeper`   - one tick: read both, plan, send
//!   `chain`    - the oracle program's accounts and instructions, over RPC or litesvm
//!   `fixture`  - a match's identity (a Merkle root over each provider's own ID)
//!   `crank`    - the permissionless transactions that move a market through its life

pub mod chain;
pub mod crank;
pub mod fixture;
pub mod keeper;
pub mod model;
pub mod planner;
pub mod sources;
