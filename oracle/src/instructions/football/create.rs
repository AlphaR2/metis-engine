use pinocchio::{
    AccountView, Address, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, rent::Rent},
};

use pinocchio_system::instructions::CreateAccount;

use crate::{errors::OracleError, instructions::football::FootballSettled};

pub fn process_create_football(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // load accounts
    let [creator, account_to_create, _system_program, _extra @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let football_length = FootballSettled::LEN;
    let football_data = data
        .get(..football_length)
        .ok_or(OracleError::InvalidFootballData)?;

    let received_data_serialised =
        bytemuck::checked::try_from_bytes::<FootballSettled>(football_data)
            .map_err(|_| OracleError::InvalidFootballData)?;

    // deriving bump onchain
    let bump = {
        let pda_seeds: [&[u8]; 3] = [
            b"football",
            b"settled",
            &received_data_serialised.fixture_merkle_root,
        ];
        let try_create_account = Address::derive_program_address(&pda_seeds, &crate::ID)
            .ok_or(OracleError::InvalidOraclePda)?;

        // assert derived onchain and offchain accounts are equal
        if !try_create_account.0.eq(account_to_create.address()) {
            return Err(OracleError::InvalidOraclePda.into());
        }

        &[try_create_account.1]
    };

    let seeds = [
        Seed::from(b"football"),
        Seed::from(b"settled"),
        Seed::from(&received_data_serialised.fixture_merkle_root),
        Seed::from(bump),
    ];

    let signer = Signer::from(&seeds);

    let rent = Rent::get()?;
    let rent_exempt = rent
        .try_minimum_balance(football_length as usize)
        .map_err(|_| OracleError::RentCalculationFailed)?;

    // log!("size ser is: {}", size);
    // The fixture Merkle root is immutable and doubles as this PDA's identity.

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
