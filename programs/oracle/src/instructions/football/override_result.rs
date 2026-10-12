use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{
    AccountView, ProgramResult,
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock},
};
use shank::ShankType;

use crate::{
    constants::{FOOTBALL_SEED, FOOTBALL_SETTLED_DISCRIMINATOR, SETTLED_SEED},
    error::OracleError,
    instructions::football::{CFootballMatchPhase, Claim, FootballSettled, OverrideAction},
    validation::{
        derive_and_check_pda, load_root_authority, require_discriminator, require_not_paused,
        require_owned_by, require_two_root_signers, require_writable, unpack_mut,
    },
};

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy, ShankType)]
pub struct OverrideResultArgs {
    /// 0 = replace the result, 1 = void it.
    pub action: u8,
    pub status: CFootballMatchPhase,
    pub score: u8,
    pub penalties: u8,
    /// The 90-minute score, stated explicitly: an override can't derive it from a phase change.
    pub regulation_score: u8,
}

impl OverrideResultArgs {
    pub const LEN: usize = core::mem::size_of::<Self>();
}

/// The escape hatch for a result the reporters got wrong or never agreed on. Two distinct root
/// signers, and only until OVERRIDE_WINDOW_SECS after the result was finalised - so it can
/// repair a bad result inside every market's dispute window but can't rewrite history later.
///
/// Accounts:
///   0 first_root   (signer)
///   1 second_root  (signer, a different root)
///   2 oracle       (writable)
///   3 authority    (the oracle's RootAuthority PDA)
pub fn process_override_result(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let [first_root, second_root, oracle, authority, _extra @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    require_writable(oracle, OracleError::OracleShouldNotBeEmpty)?;

    let root = load_root_authority(authority)?;
    require_not_paused(root)?;
    require_two_root_signers(root, first_root, second_root)?;

    if data.len() != OverrideResultArgs::LEN {
        return Err(ProgramError::InvalidInstructionData);
    }
    let args = bytemuck::checked::try_from_bytes::<OverrideResultArgs>(data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    let action = OverrideAction::try_from(args.action)?;

    require_owned_by(oracle, &crate::ID, OracleError::OracleMustBeSystemOwned)?;
    // Read the address first: the account is mutably borrowed for the state below.
    let oracle_address = *oracle.address();
    let football = unpack_mut::<FootballSettled>(
        unsafe { oracle.borrow_unchecked_mut() },
        OracleError::InvalidFootballData,
    )?;
    require_discriminator(football.discriminator, FOOTBALL_SETTLED_DISCRIMINATOR)?;
    derive_and_check_pda(
        &[FOOTBALL_SEED, SETTLED_SEED, &football.fixture_merkle_root],
        &crate::ID,
        &oracle_address,
        OracleError::InvalidOraclePda,
    )?;

    let now = Clock::get()?.unix_timestamp;
    football.apply_override(
        action,
        Claim { status: args.status, score: args.score, penalties: args.penalties },
        args.regulation_score,
        now,
    )
}
