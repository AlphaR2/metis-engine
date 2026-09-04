use pinocchio::{AccountView, Address, ProgramResult, error::ProgramError};

use crate::{
    errors::OracleError,
    instructions::authority::{AUTHORITY_SEED, INDIVIDUAL_AUTHORITY_SEED},
    state::{IndividualOAuthority, OracleAuthority},
};

pub fn process_remove_authority(accounts: &mut [AccountView]) -> ProgramResult {
    let [
        root,
        authority,
        candidate,
        individual_authority,
        recipient,
        _extra @ ..,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !root.is_signer() {
        return Err(OracleError::CreatorMustSign.into());
    }

    let (expected_authority, _) = Address::derive_program_address(&[AUTHORITY_SEED], &crate::ID)
        .ok_or(OracleError::InvalidAuthorityPda)?;
    if expected_authority != *authority.address() || !authority.owned_by(&crate::ID) {
        return Err(OracleError::InvalidAuthorityPda.into());
    }

    let authority_state = bytemuck::checked::try_from_bytes::<OracleAuthority>(unsafe {
        authority.borrow_unchecked()
    })
    .map_err(|_| OracleError::InvalidAuthority)?;
    if !authority_state.is_supermajority(*root.address()) {
        return Err(OracleError::UnauthorizedAuthority.into());
    }

    let (expected_individual_authority, _) = Address::derive_program_address(
        &[INDIVIDUAL_AUTHORITY_SEED, candidate.address().as_ref()],
        &crate::ID,
    )
    .ok_or(OracleError::InvalidAuthorityPda)?;
    if expected_individual_authority != *individual_authority.address()
        || !individual_authority.owned_by(&crate::ID)
    {
        return Err(OracleError::InvalidAuthorityPda.into());
    }

    let individual_state = bytemuck::checked::try_from_bytes::<IndividualOAuthority>(unsafe {
        individual_authority.borrow_unchecked()
    })
    .map_err(|_| OracleError::InvalidAuthority)?;
    if individual_state.address != candidate.address().to_bytes() {
        return Err(OracleError::InvalidAuthority.into());
    }

    let recipient_lamports = recipient
        .lamports()
        .checked_add(individual_authority.lamports())
        .ok_or(ProgramError::InvalidArgument)?;
    recipient.set_lamports(recipient_lamports);
    individual_authority.set_lamports(0);
    individual_authority.close()
}
