use metis_authority::instructions::remove_authority;
use pinocchio::{AccountView, ProgramResult};

use crate::{
    config::state::{CONFIG_DISCRIMINATOR, Config, DELEGATED_AUTHORITY_DISCRIMINATOR},
    constants::{CONFIG_SEED, DELEGATED_AUTHORITY_SEED, GUARDIAN},
    error::MetisError,
};

pub fn process_remove_authority(accounts: &mut [AccountView]) -> ProgramResult {
    remove_authority::<Config, MetisError>(
        accounts,
        &crate::ID,
        &[CONFIG_SEED, GUARDIAN.as_ref()],
        DELEGATED_AUTHORITY_SEED,
        CONFIG_DISCRIMINATOR,
        DELEGATED_AUTHORITY_DISCRIMINATOR,
    )
}
