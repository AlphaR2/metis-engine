//! Shared by the DBC-backed handlers (activate_market, claim_backer). The arithmetic and the escrow address
//! are in the `metis-dbc` crate; what needs the program's own id stays here.

use pinocchio::{Address, error::ProgramError};

use crate::constants::OUTCOME_MINT_SEED;

pub use metis_dbc::{ata_address, pro_rata};

/// The market's outcome mint for `index`, as initialize_market derived it.
pub fn outcome_mint_address(market: &Address, index: u8) -> Result<Address, ProgramError> {
    let index_byte = [index];
    let (mint, _) = Address::derive_program_address(&[OUTCOME_MINT_SEED, market.as_ref(), &index_byte], &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    Ok(mint)
}
