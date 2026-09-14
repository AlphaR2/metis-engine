use pinocchio::{AccountView, ProgramResult, error::ProgramError};

use crate::{
    constants::{
        AUTHORITY_SEED, INDIVIDUAL_AUTHORITY_DISCRIMINATOR, ORACLE_AUTHORITY_DISCRIMINATOR,
    },
    error::OracleError,
    state::{IndividualOAuthority, OracleAuthority},
    validation::{
        derive_and_check_pda, individual_authority_address, require_discriminator,
        require_owned_by, require_signer, unpack,
    },
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

    require_signer(root, OracleError::CreatorMustSign)?;

    derive_and_check_pda(
        &[AUTHORITY_SEED],
        &crate::ID,
        authority.address(),
        OracleError::InvalidAuthorityPda,
    )?;
    require_owned_by(authority, &crate::ID, OracleError::InvalidAuthorityPda)?;

    let authority_state = unsafe {
        unpack::<OracleAuthority>(authority.borrow_unchecked(), OracleError::InvalidAuthority)?
    };
    require_discriminator(authority_state.discriminator, ORACLE_AUTHORITY_DISCRIMINATOR)?;
    if !authority_state.is_supermajority(*root.address()) {
        return Err(OracleError::UnauthorizedAuthority.into());
    }

    let (expected_individual_authority, _) =
        individual_authority_address(candidate.address(), &crate::ID)?;
    if expected_individual_authority != *individual_authority.address() {
        return Err(OracleError::InvalidAuthorityPda.into());
    }
    require_owned_by(individual_authority, &crate::ID, OracleError::InvalidAuthorityPda)?;

    let individual_state = unsafe {
        unpack::<IndividualOAuthority>(
            individual_authority.borrow_unchecked(),
            OracleError::InvalidAuthority,
        )?
    };
    require_discriminator(individual_state.discriminator, INDIVIDUAL_AUTHORITY_DISCRIMINATOR)?;
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
