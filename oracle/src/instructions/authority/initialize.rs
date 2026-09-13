use pinocchio::{
    AccountView, Address, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, rent::Rent},
};
use pinocchio_system::instructions::CreateAccount;

use crate::{errors::OracleError, instructions::authority::AUTHORITY_SEED, state::OracleAuthority};

pub fn process_initialize_authority(accounts: &mut [AccountView]) -> ProgramResult {
    let [initializer, authority, _system_program, _extra @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let authority_state = OracleAuthority::hardcoded();
    if !initializer.is_signer() || !authority_state.is_supermajority(*initializer.address()) {
        return Err(OracleError::UnauthorizedAuthority.into());
    }

    if !authority.owned_by(&pinocchio_system::ID) || !authority.is_data_empty() {
        return Err(OracleError::OracleAlreadyInitialized.into());
    }

    let (expected_authority, bump) = Address::derive_program_address(&[AUTHORITY_SEED], &crate::ID)
        .ok_or(OracleError::InvalidAuthorityPda)?;
    if expected_authority != *authority.address() {
        return Err(OracleError::InvalidAuthorityPda.into());
    }

    let bump = [bump];
    let seeds = [Seed::from(AUTHORITY_SEED), Seed::from(&bump)];
    let signer = Signer::from(&seeds);
    let rent_exempt = Rent::get()?
        .try_minimum_balance(OracleAuthority::LEN)
        .map_err(|_| OracleError::RentCalculationFailed)?;

    CreateAccount {
        from: initializer,
        lamports: rent_exempt,
        owner: &crate::ID,
        space: OracleAuthority::LEN as u64,
        to: authority,
    }
    .invoke_signed(&[signer])?;

    let authority_data = unsafe { authority.borrow_unchecked_mut() };
    authority_data.copy_from_slice(bytemuck::bytes_of(&authority_state));
    Ok(())
}
