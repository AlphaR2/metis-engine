
#![allow(unexpected_cfgs)]

use pinocchio::{
    AccountView, Address, ProgramResult, address::declare_id, error::ProgramError,
};

use crate::instructions::POracle;

// Public so the keeper (tools/oracle-keeper) and metis-core can reuse the real types, constants
// and layouts instead of keeping hand-copied versions that could drift.
pub mod constants;
pub mod error;
pub mod instructions;
pub mod state;
mod tests;
mod validation;

// Off when another crate links this one as a library (the aggregator reads layouts and runs
// the curve math off-chain); two entrypoints in one binary would collide.
#[cfg(not(feature = "no-entrypoint"))]
pinocchio::entrypoint!(process_instruction);

declare_id!("BM7jwCCXjBgRy5LPg1UG4iAPueAFK3bRxQRogD4Wzoub");

pub fn process_instruction(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    if program_id != &ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    let (discriminator, data) = instruction_data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;

    match POracle::try_from(discriminator)? {
        POracle::CreateOracle => instructions::process_create_oracle(accounts, data),
        POracle::SubmitResult => instructions::process_submit_result(accounts, data),
        POracle::CloseOracle => instructions::process_close_oracle(accounts, data),
        POracle::InitializeAuthority => instructions::process_initialize_authority(accounts),
        POracle::AttestAuthority => instructions::process_attest_authority(accounts),
        POracle::RemoveAuthority => instructions::process_remove_authority(accounts),
        POracle::OverrideResult => instructions::process_override_result(accounts, data),
        POracle::Pause => instructions::process_pause(accounts),
        POracle::Unpause => instructions::process_unpause(accounts),
    }
}
