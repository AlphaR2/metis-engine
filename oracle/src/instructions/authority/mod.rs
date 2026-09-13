pub const AUTHORITY_SEED: &[u8] = b"oracle_authority";
pub const INDIVIDUAL_AUTHORITY_SEED: &[u8] = b"individual_authority";

use pinocchio::{AccountView, ProgramResult};

use crate::{
    errors::OracleError,
    state::{IndividualOAuthority, SUPERMAJORITIES},
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

    let individual_state = bytemuck::checked::try_from_bytes::<IndividualOAuthority>(unsafe {
        caller_individual_authority.borrow_unchecked()
    })
    .map_err(|_| OracleError::UnauthorizedAuthority)?;
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
