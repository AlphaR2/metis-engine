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
    instructions::{
        authority::assert_oracle_updater,
        football::{CFootballMatchPhase, Claim, FootballSettled},
    },
    validation::{
        derive_and_check_pda, load_root_authority, require_discriminator, require_not_paused,
        require_owned_by, require_signer, require_writable, unpack_mut,
    },
};

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy, ShankType)]
pub struct SubmitResultArgs {
    pub status: CFootballMatchPhase,
    /// Packed: high nibble home, low nibble away.
    pub score: u8,
    /// Packed the same way. Zero before the Penalties phase.
    pub penalties: u8,
}

impl SubmitResultArgs {
    pub const LEN: usize = core::mem::size_of::<Self>();
}

/// One reporter states what it sees. The oracle records it, and applies it only once
/// REQUIRED_REPORTS distinct reporters have said exactly the same thing.
///
/// Accounts:
///   0 reporter              (signer - root, or an attested individual authority)
///   1 oracle                (writable, the FootballSettled PDA)
///   2 authority             (the oracle's RootAuthority PDA)
///   3 individual_authority  (the reporter's own attestation; required unless the reporter is root)
pub fn process_submit_result(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let [reporter, oracle, authority, individual_authority @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if individual_authority.len() > 1 {
        return Err(ProgramError::InvalidInstructionData);
    }
    require_signer(reporter, OracleError::CreatorMustSign)?;
    require_writable(oracle, OracleError::OracleShouldNotBeEmpty)?;

    let root = load_root_authority(authority)?;
    require_not_paused(root)?;
    assert_oracle_updater(reporter, root, individual_authority.first())?;

    if data.len() != SubmitResultArgs::LEN {
        return Err(ProgramError::InvalidInstructionData);
    }
    let args = bytemuck::checked::try_from_bytes::<SubmitResultArgs>(data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;

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
    football.submit(
        reporter.address().to_bytes(),
        Claim { status: args.status, score: args.score, penalties: args.penalties },
        now,
    )?;
    Ok(())
}
