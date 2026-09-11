//! Scaffold only — explicitly deferred, see the root README and the
//! restructure plan. `settle_market` currently checks a single hardcoded
//! `constants::AUTHORITY` pubkey, a stand-in for a real oracle. This crate
//! is where that gets replaced with a real one: a genuinely external
//! integration (matching how Drift treats Pyth/Switchboard as separate
//! programs it CPIs into), not an internal module of `metis-core`.
//!
//! No logic here yet. When this gets built, the shape is roughly:
//! a trait describing "who won outcome X for market Y," with concrete
//! implementations (a multisig-of-reporters, a CPI to a real price/result
//! oracle, etc.) behind it.

pub trait OutcomeOracle {
    /// Returns the winning outcome index for a resolved event, if known.
    fn resolve(&self, market_identifier: &[u8]) -> Option<u8>;
}
