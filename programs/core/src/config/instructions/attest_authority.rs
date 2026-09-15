use metis_authority::instructions::attest_authority;
use pinocchio::{AccountView, ProgramResult};

use crate::{
    config::state::{CONFIG_DISCRIMINATOR, Config, DELEGATED_AUTHORITY_DISCRIMINATOR},
    constants::{CONFIG_SEED, DELEGATED_AUTHORITY_SEED, GUARDIAN},
    error::MetisError,
};

pub fn process_attest_authority(accounts: &mut [AccountView]) -> ProgramResult {
    attest_authority::<Config, MetisError>(
        accounts,
        &crate::ID,
        &[CONFIG_SEED, GUARDIAN.as_ref()],
        DELEGATED_AUTHORITY_SEED,
        CONFIG_DISCRIMINATOR,
        DELEGATED_AUTHORITY_DISCRIMINATOR,
    )
}
