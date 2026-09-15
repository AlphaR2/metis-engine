use metis_authority::delegated::DelegatedAuthority;
use pinocchio::{AccountView, ProgramResult};

use crate::{
    config::state::{Config, DELEGATED_AUTHORITY_DISCRIMINATOR},
    error::MetisError,
    validation::{require_discriminator, unpack},
};

/// Confirms that `candidate` is either root or a currently-attested DelegatedAuthority 
/// Used two ways: at initialize_market, to validate whoever a creator is assigning as a
/// market's delegate is actually legitimate; 
/// and at settle_market, to check the signer against
/// root or the market's own recorded delegate.
pub fn assert_amm_authority(
    candidate: &AccountView,
    candidate_delegated_authority: Option<&AccountView>,
    config: &Config,
) -> ProgramResult {
    if config.root.is_root(*candidate.address()) {
        return Ok(());
    }

    let candidate_delegated_authority =
        candidate_delegated_authority.ok_or(MetisError::UnauthorizedAuthority)?;
    if !candidate_delegated_authority.owned_by(&crate::ID) {
        return Err(MetisError::UnauthorizedAuthority.into());
    }

    let delegated_state = unpack::<DelegatedAuthority>(
        unsafe { candidate_delegated_authority.borrow_unchecked() },
        MetisError::UnauthorizedAuthority,
    )?;
    require_discriminator(delegated_state.discriminator, DELEGATED_AUTHORITY_DISCRIMINATOR)?;
    if delegated_state.address != candidate.address().to_bytes()
        || !delegated_state.is_attest_complete()
    {
        return Err(MetisError::UnauthorizedAuthority.into());
    }

    Ok(())
}

pub mod attest_authority;
pub mod initialize_config;
pub mod remove_authority;
pub mod update_config;
