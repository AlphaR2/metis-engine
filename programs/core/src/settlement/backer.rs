//! Shared by the DBC-backed paths (activate_market, claim_backer): where the seed shares of a
//! backed market live, and how a backer's slice of anything is computed.

use pinocchio::{Address, error::ProgramError};

use crate::constants::OUTCOME_MINT_SEED;

/// The associated token account of `owner` for `mint`: where a backed market's seed shares are
/// escrowed (owner = the market PDA). Derived rather than passed in, so activation and every later
/// claim agree on one account per outcome.
pub fn ata_address(owner: &Address, mint: &Address) -> Result<Address, ProgramError> {
    let (ata, _) = Address::derive_program_address(
        &[owner.as_ref(), pinocchio_token::ID.as_ref(), mint.as_ref()],
        &pinocchio_associated_token_account::ID,
    )
    .ok_or(ProgramError::InvalidSeeds)?;
    Ok(ata)
}

/// The market's outcome mint for `index`, as initialize_market derived it.
pub fn outcome_mint_address(market: &Address, index: u8) -> Result<Address, ProgramError> {
    let index_byte = [index];
    let (mint, _) = Address::derive_program_address(&[OUTCOME_MINT_SEED, market.as_ref(), &index_byte], &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    Ok(mint)
}

/// `amount * tokens / supply`, rounded down: a backer's slice of `amount` when they redeem
/// `tokens` of the `supply` still outstanding. Because the backer's tokens are burned and the
/// slice leaves `amount`, the ratio of what remains to what is outstanding never changes, so every
/// token is worth the same whenever it is redeemed. Rounding down leaves dust, never a shortfall.
pub fn pro_rata(amount: u64, tokens: u64, supply: u64) -> u64 {
    if supply == 0 || tokens == 0 {
        return 0;
    }
    let tokens = tokens.min(supply);
    ((amount as u128) * (tokens as u128) / (supply as u128)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn slice_is_proportional_and_floors() {
        assert_eq!(pro_rata(100, 25, 100), 25);
        assert_eq!(pro_rata(10, 1, 3), 3);
        assert_eq!(pro_rata(10, 0, 3), 0);
        assert_eq!(pro_rata(10, 5, 0), 0);
    }

    #[test]
    fn redeeming_everything_pays_at_most_the_amount() {
        // Three backers redeem in sequence against a shrinking pot and a shrinking supply.
        let (mut pot, mut supply) = (1_000_001u64, 1_000u64);
        let mut paid = 0u64;
        for tokens in [333u64, 333, 334] {
            let slice = pro_rata(pot, tokens, supply);
            paid += slice;
            pot -= slice;
            supply -= tokens;
        }
        assert!(paid <= 1_000_001);
        assert!(1_000_001 - paid <= 3, "only rounding dust is left behind");
    }

    proptest! {
        #[test]
        fn a_redeemer_never_takes_more_than_their_fraction(
            amount in 0u64..1_000_000_000_000,
            supply in 1u64..1_000_000_000_000,
            frac in 0u64..=1_000,
        ) {
            let tokens = supply / 1_000 * frac;
            let slice = pro_rata(amount, tokens, supply);
            prop_assert!(slice <= amount);
            prop_assert!((slice as u128) * (supply as u128) <= (amount as u128) * (tokens as u128));
        }
    }
}
