use pinocchio::{
    AccountView, Address, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, rent::Rent},
};
use pinocchio_log::log;
use pinocchio_system::instructions::CreateAccount;
use wincode::Deserialize;

use crate::errors::OracleError;
use crate::state::football_settled::Football;

pub fn process_wincode_create_football(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // load accounts
    let [creator, account_to_create, _system_program, _extra @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // signer checks
    if !creator.is_signer() {
        return Err(OracleError::CreatorMustSign.into());
    }
    // account ownership checks
    if !account_to_create.owned_by(_system_program.address()) {
        return Err(OracleError::OracleMustBeSystemOwned.into());
    }

    // expected account state check
    if !account_to_create.is_data_empty() {
        return Err(OracleError::OracleAlreadyInitialized.into());
    }

    let football_length = Football::default().length();
    let football_data = data
        .get(..football_length)
        .ok_or(OracleError::InvalidFootballData)?;
    let received_data_serialised: Football =
        Football::deserialize(football_data).map_err(|_| OracleError::InvalidFootballData)?;

    // let size = wincode::serialized_size(&received_data_serialised)
    //     .map_err(|_| OracleError::InvalidFootballData)?;

    // deriving bump onchain
    let bump = {
        let pda_seeds: [&[u8]; 3] = [b"football", b"settled", &received_data_serialised.match_id];
        let try_create_account = Address::derive_program_address(&pda_seeds, &crate::ID)
            .ok_or(OracleError::InvalidOraclePda)?;

        // assert derived onchain and offchain accounts are equal
        if try_create_account.0 != *account_to_create.address() {
            return Err(OracleError::InvalidOraclePda.into());
        }

        &[try_create_account.1]
    };

    let seeds = [
        Seed::from(b"football"),
        Seed::from(b"settled"),
        Seed::from(&received_data_serialised.match_id),
        Seed::from(bump),
    ];

    let signer = Signer::from(&seeds);

    let rent = Rent::get()?;
    let rent_exempt = rent
        .try_minimum_balance(football_length as usize)
        .map_err(|_| OracleError::RentCalculationFailed)?;

    // log!("size ser is: {}", size);
    // log!("match id is: ");
    // log!("{}", u32::from_le_bytes(received_data_serialised.match_id));

    CreateAccount {
        from: creator,
        lamports: rent_exempt,
        owner: &crate::ID,
        space: football_length as u64,
        to: account_to_create,
    }
    .invoke_signed(&[signer])?;

    // write to said account

    let borrowed_data = unsafe { account_to_create.borrow_unchecked_mut() };
    borrowed_data.copy_from_slice(&mut football_data.to_vec());
    Ok(())
}
