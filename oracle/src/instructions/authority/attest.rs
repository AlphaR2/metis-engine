use pinocchio::{
    AccountView, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, rent::Rent},
};
use pinocchio_system::instructions::CreateAccount;

use crate::{
    constants::{
        AUTHORITY_SEED, INDIVIDUAL_AUTHORITY_DISCRIMINATOR, INDIVIDUAL_AUTHORITY_SEED,
        ORACLE_AUTHORITY_DISCRIMINATOR,
    },
    error::OracleError,
    state::{IndividualOAuthority, OracleAuthority},
    validation::{
        derive_and_check_pda, individual_authority_address, require_discriminator,
        require_owned_by, require_signer, unpack, unpack_mut,
    },
};

pub fn process_attest_authority(accounts: &mut [AccountView]) -> ProgramResult {
    let [
        attester,
        authority,
        candidate,
        individual_authority,
        _system_program,
        remaining @ ..,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    require_signer(attester, OracleError::CreatorMustSign)?;

    derive_and_check_pda(
        &[AUTHORITY_SEED],
        &crate::ID,
        authority.address(),
        OracleError::InvalidAuthorityPda,
    )?;
    require_owned_by(authority, &crate::ID, OracleError::InvalidAuthorityPda)?;

    let authority_state = *unpack::<OracleAuthority>(
        unsafe { authority.borrow_unchecked() },
        OracleError::InvalidAuthority,
    )?;
    require_discriminator(authority_state.discriminator, ORACLE_AUTHORITY_DISCRIMINATOR)?;
    let attester_is_root = authority_state.is_supermajority(*attester.address());

    if !attester_is_root {
        let attester_individual_authority = remaining
            .first()
            .ok_or(ProgramError::NotEnoughAccountKeys)?;
        let (expected_attester_authority, _) =
            individual_authority_address(attester.address(), &crate::ID)?;
        if expected_attester_authority != *attester_individual_authority.address() {
            return Err(OracleError::UnauthorizedAuthority.into());
        }
        require_owned_by(
            attester_individual_authority,
            &crate::ID,
            OracleError::UnauthorizedAuthority,
        )?;

        let attester_state = unpack::<IndividualOAuthority>(
            unsafe { attester_individual_authority.borrow_unchecked() },
            OracleError::UnauthorizedAuthority,
        )?;
        require_discriminator(attester_state.discriminator, INDIVIDUAL_AUTHORITY_DISCRIMINATOR)?;
        if attester_state.address != attester.address().to_bytes()
            || !attester_state.is_attest_complete()
        {
            return Err(OracleError::UnauthorizedAuthority.into());
        }
    }

    // cannot add a supermajority - they're already added upon program init
    if authority_state.is_supermajority(*candidate.address()) {
        return Err(OracleError::InvalidAuthority.into());
    }

    let (expected_individual_authority, bump) =
        individual_authority_address(candidate.address(), &crate::ID)?;
    if expected_individual_authority != *individual_authority.address() {
        return Err(OracleError::InvalidAuthorityPda.into());
    }

    if individual_authority.is_data_empty() {
        require_owned_by(
            individual_authority,
            &pinocchio_system::ID,
            OracleError::InvalidAuthorityPda,
        )?;

        let bump = [bump];
        let seeds = [
            Seed::from(INDIVIDUAL_AUTHORITY_SEED),
            Seed::from(candidate.address().as_ref()),
            Seed::from(&bump),
        ];
        let signer = Signer::from(&seeds);
        let rent_exempt = Rent::get()?
            .try_minimum_balance(IndividualOAuthority::LEN)
            .map_err(|_| OracleError::RentCalculationFailed)?;

        CreateAccount {
            from: attester,
            lamports: rent_exempt,
            owner: &crate::ID,
            space: IndividualOAuthority::LEN as u64,
            to: individual_authority,
        }
        .invoke_signed(&[signer])?;

        let individual_data = unsafe { individual_authority.borrow_unchecked_mut() };
        individual_data.copy_from_slice(bytemuck::bytes_of(&IndividualOAuthority::new(
            *candidate.address(),
        )));
    } else {
        require_owned_by(individual_authority, &crate::ID, OracleError::InvalidAuthorityPda)?;
    }

    let individual_state = unpack_mut::<IndividualOAuthority>(
        unsafe { individual_authority.borrow_unchecked_mut() },
        OracleError::InvalidAuthority,
    )?;
    require_discriminator(individual_state.discriminator, INDIVIDUAL_AUTHORITY_DISCRIMINATOR)?;
    if individual_state.address != candidate.address().to_bytes() {
        return Err(OracleError::InvalidAuthority.into());
    }

    if !attester_is_root && !individual_state.has_supermajority_attester(&authority_state) {
        return Err(OracleError::MissingRootAttestation.into());
    }

    individual_state.attest(*attester.address())
}
