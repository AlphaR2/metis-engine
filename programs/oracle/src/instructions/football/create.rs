use pinocchio::{
    AccountView, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, rent::Rent},
};

use pinocchio_system::instructions::CreateAccount;

use crate::{
    constants::{FOOTBALL_SEED, FOOTBALL_SETTLED_DISCRIMINATOR, SETTLED_SEED},
    error::OracleError,
    instructions::football::FootballSettled,
    validation::{derive_and_check_pda, require_discriminator, unpack},
};

pub fn process_create_football(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // load accounts
    let [creator, account_to_create, _system_program, _extra @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let football_length = FootballSettled::LEN;
    let football_data = data
        .get(..football_length)
        .ok_or(OracleError::InvalidFootballData)?;

    let received_data_serialised = unpack::<FootballSettled>(
        football_data,
        OracleError::InvalidFootballData,
    )?;
    require_discriminator(
        received_data_serialised.discriminator,
        FOOTBALL_SETTLED_DISCRIMINATOR,
    )?;

    // The fixture Merkle root is immutable and doubles as this PDA's identity.
    let bump = derive_and_check_pda(
        &[
            FOOTBALL_SEED,
            SETTLED_SEED,
            &received_data_serialised.fixture_merkle_root,
        ],
        &crate::ID,
        account_to_create.address(),
        OracleError::InvalidOraclePda,
    )?;

    let bump = [bump];
    let seeds = [
        Seed::from(FOOTBALL_SEED),
        Seed::from(SETTLED_SEED),
        Seed::from(&received_data_serialised.fixture_merkle_root),
        Seed::from(&bump),
    ];

    let signer = Signer::from(&seeds);

    let rent = Rent::get()?;
    let rent_exempt = rent
        .try_minimum_balance(football_length)
        .map_err(|_| OracleError::RentCalculationFailed)?;

    // log!("size ser is: {}", size);
    // The fixture Merkle root is immutable and doubles as this PDA's identity.

    // i think creating the Account with FootballSettled::new() should be called here no?

    CreateAccount {
        from: creator,
        lamports: rent_exempt,
        owner: &crate::ID,
        space: football_length as u64,
        to: account_to_create,
    }
    .invoke_signed(&[signer])?;

    let borrowed_data = unsafe { account_to_create.borrow_unchecked_mut() };
    borrowed_data.copy_from_slice(football_data);
    Ok(())
}
