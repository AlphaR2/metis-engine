use pinocchio::{
    AccountView, Address, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, rent::Rent},
};
use pinocchio_system::instructions::CreateAccount;

use crate::{
    errors::OracleError,
    instructions::authority::{AUTHORITY_SEED, INDIVIDUAL_AUTHORITY_SEED},
    state::{IndividualOAuthority, OracleAuthority},
};

fn individual_authority_address(candidate: &Address) -> Result<(Address, u8), ProgramError> {
    Address::derive_program_address(&[INDIVIDUAL_AUTHORITY_SEED, candidate.as_ref()], &crate::ID)
        .ok_or(OracleError::InvalidAuthorityPda.into())
}

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

    if !attester.is_signer() {
        return Err(OracleError::CreatorMustSign.into());
    }

    let (expected_authority, _) = Address::derive_program_address(&[AUTHORITY_SEED], &crate::ID)
        .ok_or(OracleError::InvalidAuthorityPda)?;
    if expected_authority != *authority.address() || !authority.owned_by(&crate::ID) {
        return Err(OracleError::InvalidAuthorityPda.into());
    }

    let authority_state = *bytemuck::checked::try_from_bytes::<OracleAuthority>(unsafe {
        authority.borrow_unchecked()
    })
    .map_err(|_| OracleError::InvalidAuthority)?;
    let attester_is_root = authority_state.is_supermajority(*attester.address());

    if !attester_is_root {
        let attester_individual_authority = remaining
            .first()
            .ok_or(ProgramError::NotEnoughAccountKeys)?;
        let (expected_attester_authority, _) = individual_authority_address(attester.address())?;
        if expected_attester_authority != *attester_individual_authority.address()
            || !attester_individual_authority.owned_by(&crate::ID)
        {
            return Err(OracleError::UnauthorizedAuthority.into());
        }

        let attester_state = bytemuck::checked::try_from_bytes::<IndividualOAuthority>(unsafe {
            attester_individual_authority.borrow_unchecked()
        })
        .map_err(|_| OracleError::UnauthorizedAuthority)?;
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

    let (expected_individual_authority, bump) = individual_authority_address(candidate.address())?;
    if expected_individual_authority != *individual_authority.address() {
        return Err(OracleError::InvalidAuthorityPda.into());
    }

    if individual_authority.is_data_empty() {
        if !individual_authority.owned_by(&pinocchio_system::ID) {
            return Err(OracleError::InvalidAuthorityPda.into());
        }

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
    } else if !individual_authority.owned_by(&crate::ID) {
        return Err(OracleError::InvalidAuthorityPda.into());
    }

    let individual_state = bytemuck::checked::try_from_bytes_mut::<IndividualOAuthority>(unsafe {
        individual_authority.borrow_unchecked_mut()
    })
    .map_err(|_| OracleError::InvalidAuthority)?;
    if individual_state.address != candidate.address().to_bytes() {
        return Err(OracleError::InvalidAuthority.into());
    }

    if !attester_is_root && !individual_state.has_supermajority_attester(&authority_state) {
        return Err(OracleError::MissingRootAttestation.into());
    }

    individual_state.attest(*attester.address())
}
