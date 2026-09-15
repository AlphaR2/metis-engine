use metis_authority::instructions::initialize_authority;
use pinocchio::{AccountView, ProgramResult};

use crate::{
    constants::{AUTHORITY_SEED, GUARDIAN, ORACLE_AUTHORITY_DISCRIMINATOR, ROOT_SIGNERS},
    error::OracleError,
};

pub fn process_initialize_authority(accounts: &mut [AccountView]) -> ProgramResult {
    initialize_authority::<OracleError>(
        accounts,
        &crate::ID,
        &[AUTHORITY_SEED, GUARDIAN.as_ref()],
        &ROOT_SIGNERS,
        GUARDIAN,
        ORACLE_AUTHORITY_DISCRIMINATOR,
    )
}
