use metis_authority::instructions::remove_authority;
use pinocchio::{AccountView, ProgramResult};

use crate::{
    constants::{
        AUTHORITY_SEED, GUARDIAN, INDIVIDUAL_AUTHORITY_DISCRIMINATOR, INDIVIDUAL_AUTHORITY_SEED,
        ORACLE_AUTHORITY_DISCRIMINATOR,
    },
    error::OracleError,
    state::RootAuthority,
};

pub fn process_remove_authority(accounts: &mut [AccountView]) -> ProgramResult {
    remove_authority::<RootAuthority, OracleError>(
        accounts,
        &crate::ID,
        &[AUTHORITY_SEED, GUARDIAN.as_ref()],
        INDIVIDUAL_AUTHORITY_SEED,
        ORACLE_AUTHORITY_DISCRIMINATOR,
        INDIVIDUAL_AUTHORITY_DISCRIMINATOR,
    )
}
