use pinocchio::{
    AccountView, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, rent::Rent},
};
use pinocchio_system::instructions::CreateAccount;

use crate::{
    constants::AUTHORITY_SEED,
    error::OracleError,
    state::OracleAuthority,
    validation::{derive_and_check_pda, require_empty, require_owned_by, require_signer},
};

pub fn process_initialize_authority(accounts: &mut [AccountView]) -> ProgramResult {
    let [initializer, authority, _system_program, _extra @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let authority_state = OracleAuthority::hardcoded();
    require_signer(initializer, OracleError::UnauthorizedAuthority)?;
    if !authority_state.is_supermajority(*initializer.address()) {
        return Err(OracleError::UnauthorizedAuthority.into());
    }

    require_owned_by(
        authority,
        &pinocchio_system::ID,
        OracleError::OracleAlreadyInitialized,
    )?;
    require_empty(authority, OracleError::OracleAlreadyInitialized)?;

    let bump = derive_and_check_pda(
        &[AUTHORITY_SEED],
        &crate::ID,
        authority.address(),
        OracleError::InvalidAuthorityPda,
    )?;

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
