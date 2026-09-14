use pinocchio::{AccountView, ProgramResult};

use crate::{
    constants::{INDIVIDUAL_AUTHORITY_DISCRIMINATOR, SUPERMAJORITIES},
    error::OracleError,
    state::IndividualOAuthority,
    validation::{require_discriminator, unpack},
};

/// Confirms that `caller` is either a root or an individually attested authority.
///
/// Account order in the create/update routes is:
/// `[caller, oracle, system_program, caller_individual_authority?]`.
pub fn assert_oracle_updater(
    caller: &AccountView,
    caller_individual_authority: Option<&AccountView>,
) -> ProgramResult {
    if SUPERMAJORITIES.contains(&caller.address().to_bytes()) {
        return Ok(());
    }

    let caller_individual_authority =
        caller_individual_authority.ok_or(OracleError::UnauthorizedAuthority)?;
    if !caller_individual_authority.owned_by(&crate::ID) {
        return Err(OracleError::UnauthorizedAuthority.into());
    }

    let individual_state = unpack::<IndividualOAuthority>(
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
