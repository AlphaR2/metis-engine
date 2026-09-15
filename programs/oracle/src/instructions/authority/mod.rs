use pinocchio::{AccountView, ProgramResult};

use crate::{
    constants::INDIVIDUAL_AUTHORITY_DISCRIMINATOR,
    error::OracleError,
    state::{DelegatedAuthority, RootAuthority},
    validation::{require_discriminator, unpack},
};

/// Confirms that `caller` is either root or an individually attested authority - checked
/// against the actual on-chain RootAuthority account (not a compiled-in constant), so root
/// rotation and the pause flag apply here exactly the same as they already do for
/// attest/remove/close_oracle.
pub fn assert_oracle_updater(
    caller: &AccountView,
    authority: &RootAuthority,
    caller_individual_authority: Option<&AccountView>,
) -> ProgramResult {
    if authority.is_root(*caller.address()) {
        return Ok(());
    }

    let caller_individual_authority =
        caller_individual_authority.ok_or(OracleError::UnauthorizedAuthority)?;
    if !caller_individual_authority.owned_by(&crate::ID) {
        return Err(OracleError::UnauthorizedAuthority.into());
    }

    let individual_state = unpack::<DelegatedAuthority>(
        unsafe { caller_individual_authority.borrow_unchecked() },
        OracleError::UnauthorizedAuthority,
    )?;
    require_discriminator(individual_state.discriminator, INDIVIDUAL_AUTHORITY_DISCRIMINATOR)?;
    if individual_state.address != caller.address().to_bytes()
        || !individual_state.is_attest_complete()
    {
        return Err(OracleError::UnauthorizedAuthority.into());
    }

    Ok(())
}

pub mod initialize;
pub use initialize::*;

pub mod attest;
pub use attest::*;

pub mod remove;
pub use remove::*;
