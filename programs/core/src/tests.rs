// Cross-part checks that don't belong to any single part's own state.rs,
// since they need visibility into more than one part's discriminators at
// once — this is the one place that's true.

use crate::{amm, settlement};

#[test]
fn discriminators_are_all_distinct_across_every_part() {
    let account_discs: [[u8; 8]; 3] = [
        amm::state::MARKET_DISCRIMINATOR,
        amm::state::POSITION_DISCRIMINATOR,
        settlement::state::VAULT_DISCRIMINATOR,
    ];
    for i in 0..account_discs.len() {
        for j in (i + 1)..account_discs.len() {
            assert_ne!(
                account_discs[i], account_discs[j],
                "account discriminators {i} and {j} collide"
            );
        }
    }

    let ix_discs: [[u8; 8]; 4] = [
        amm::state::INITIALIZE_MARKET,
        amm::state::PLACE_BET,
        settlement::state::SETTLE_MARKET,
        settlement::state::CLAIM,
    ];
    for i in 0..ix_discs.len() {
        for j in (i + 1)..ix_discs.len() {
            assert_ne!(ix_discs[i], ix_discs[j], "instruction discriminators {i} and {j} collide");
        }
    }
}
