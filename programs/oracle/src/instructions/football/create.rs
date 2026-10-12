use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{
    AccountView, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock, rent::Rent},
};
use pinocchio_system::instructions::CreateAccount;

use crate::{
    constants::{FOOTBALL_SEED, MIN_KICKOFF_LEAD_SECS, SETTLED_SEED},
    error::OracleError,
    instructions::football::FootballSettled,
    validation::derive_and_check_pda,
};

/// What a caller supplies to create a fixture: who it is and when it starts. Nothing else - the
/// score, status and reports always start empty, so an oracle can never be created already
/// finished or already scored.
#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy)]
pub struct CreateFootballArgs {
    pub fixture_merkle_root: [u8; 32],
    /// i64 LE, unix seconds.
    pub kickoff_ts: [u8; 8],
}

impl CreateFootballArgs {
    pub const LEN: usize = core::mem::size_of::<Self>();
}

pub fn process_create_football(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // load accounts
    let [creator, account_to_create, _system_program, _extra @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let args_bytes = data
        .get(..CreateFootballArgs::LEN)
        .ok_or(OracleError::InvalidFootballData)?;
    let args = bytemuck::checked::try_from_bytes::<CreateFootballArgs>(args_bytes)
        .map_err(|_| OracleError::InvalidFootballData)?;

    let now = Clock::get()?.unix_timestamp;
    let kickoff = i64::from_le_bytes(args.kickoff_ts);
    if kickoff < now.saturating_add(MIN_KICKOFF_LEAD_SECS) {
        return Err(OracleError::KickoffTooSoon.into());
    }

    // The fixture Merkle root is immutable and doubles as this PDA's identity.
    let bump = derive_and_check_pda(
        &[FOOTBALL_SEED, SETTLED_SEED, &args.fixture_merkle_root],
        &crate::ID,
        account_to_create.address(),
        OracleError::InvalidOraclePda,
    )?;

    let bump = [bump];
    let seeds = [
        Seed::from(FOOTBALL_SEED),
        Seed::from(SETTLED_SEED),
        Seed::from(&args.fixture_merkle_root),
        Seed::from(&bump),
    ];
    let signer = Signer::from(&seeds);

    let rent_exempt = Rent::get()?
        .try_minimum_balance(FootballSettled::LEN)
        .map_err(|_| OracleError::RentCalculationFailed)?;

    CreateAccount {
        from: creator,
        lamports: rent_exempt,
        owner: &crate::ID,
        space: FootballSettled::LEN as u64,
        to: account_to_create,
    }
    .invoke_signed(&[signer])?;

    let fresh = FootballSettled::new(args.fixture_merkle_root, kickoff, now);
    let borrowed_data = unsafe { account_to_create.borrow_unchecked_mut() };
    borrowed_data.copy_from_slice(fresh.as_bytes());
    Ok(())
}
