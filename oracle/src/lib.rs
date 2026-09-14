
#![allow(unexpected_cfgs)]

use pinocchio::{
    AccountView, Address, ProgramResult, address::declare_id, entrypoint, error::ProgramError,
};

use crate::instructions::POracle;

mod constants;
mod error;
mod instructions;
mod state;
mod tests;
mod validation;

entrypoint!(process_instruction);

declare_id!("PorC1q7btDTY3jyrNxCgieXU5a4nGzhchjjnThYM3un");

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
        POracle::UpdateOracle => instructions::process_update_oracle(accounts, data),
        POracle::InitializeAuthority => instructions::process_initialize_authority(accounts),
        POracle::AttestAuthority => instructions::process_attest_authority(accounts),
        POracle::RemoveAuthority => instructions::process_remove_authority(accounts),

        _ => Err(ProgramError::InvalidInstructionData),
    }
}
