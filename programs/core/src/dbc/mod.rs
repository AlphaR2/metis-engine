//! Meteora's Dynamic Bonding Curve (DBC) in Metis: the **Open Pool**.
//!
//! A market normally opens with a creator-paid seed. With the Open Pool, a crowd raises that seed instead, by
//! buying a token on a Meteora DBC pool; when the pool fills, the raised USDC funds the market's vault and the
//! market opens. This module holds every instruction that exists only for that flow:
//!
//! | Instruction | File | What it does |
//! |---|---|---|
//! | `initialize_backed_market` | [`initialize_backed_market`] | Opens the market `Pending`, recording the backer token, seed target and deadline |
//! | `activate_market` | [`activate_market`] | An operator opens it once the vault holds the seed and the backer token is final |
//! | `cancel_pending_market` | [`cancel_pending_market`] | Cancels a raise that missed its deadline (backers sell back on the curve) |
//! | `claim_backer` | [`claim_backer`] | A backer redeems their tokens for their share of the escrowed seed shares |
//!
//! [`backer`] holds the redemption arithmetic.
//!
//! **This program never calls Meteora.** The pool, the migration to DAMM v2, and moving the liquidity into the
//! vault all happen off-chain, in `dbc/keeper`; `activate_market` only checks the result (the seed is
//! in the vault, the backer token has no mint or freeze authority). The feature is switched by
//! `Config.dbc_backing_enabled`; with it off every market is creator-seeded and none of this runs.
//!
//! The market account fields it uses (`seed_target`, `backer_mint`, `raise_deadline`, `seed_source`) and the
//! `Pending` status live with the rest of the market state in `amm::state`. Instruction discriminators are
//! generated into `amm::state` and `settlement::state` (do not hand-edit; see `tools/compute-discriminators`)
//! and are re-exported here so the whole DBC surface can be found in one place.

pub mod activate_market;
pub mod backer;
pub mod cancel_pending_market;
pub mod claim_backer;
pub mod initialize_backed_market;

pub use crate::amm::state::{ACTIVATE_MARKET, INITIALIZE_BACKED_MARKET};
pub use crate::settlement::state::{CANCEL_PENDING_MARKET, CLAIM_BACKER};

#[cfg(test)]
mod tests {
    use super::*;

    /// The instruction ids are part of the deployed program's interface: clients, keepers and the aggregator
    /// build transactions with these exact bytes. Moving code between modules must never change them.
    #[test]
    fn the_dbc_instruction_ids_never_move() {
        assert_eq!(INITIALIZE_BACKED_MARKET, [231, 227, 24, 84, 60, 60, 80, 227]);
        assert_eq!(ACTIVATE_MARKET, [10, 26, 197, 116, 113, 99, 72, 89]);
        assert_eq!(CLAIM_BACKER, [172, 148, 83, 118, 116, 185, 190, 133]);
        assert_eq!(CANCEL_PENDING_MARKET, [187, 109, 220, 223, 176, 26, 179, 80]);
    }
}
