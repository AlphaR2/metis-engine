#![allow(unexpected_cfgs)]
use pinocchio::{
    AccountView, Address, ProgramResult, address::declare_id, entrypoint, error::ProgramError,
};

pub mod amm;
pub mod clob;
pub mod config;
pub mod constants;
pub mod error;
pub mod settlement;
pub mod validation;

#[cfg(test)]
mod tests;

declare_id!("2jt26iYKHghHDFVWQ8rnCdQc5B144Cos6HyJVw68qiv6");

entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Address,
    accounts: &mut [AccountView],
    ix_data: &[u8],
) -> ProgramResult {
    if program_id != &ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    let (discriminator, data) = ix_data
        .split_first_chunk::<8>()
        .ok_or(ProgramError::InvalidInstructionData)?;

    match *discriminator {
        amm::state::INITIALIZE_MARKET => {
            amm::instructions::initialize_market::process_initialize_market(accounts, data)
        }
        amm::state::PLACE_BET => amm::instructions::place_bet::process_place_bet(accounts, data),
        settlement::state::SETTLE_MARKET => {
            settlement::instructions::settle_market::process_settle_market(accounts, data)
        }
        settlement::state::CLAIM => settlement::instructions::claim::process_claim(accounts, data),
        config::state::INITIALIZE_CONFIG => {
            config::instructions::initialize_config::process_initialize_config(accounts, data)
        }
        config::state::UPDATE_CONFIG => {
            config::instructions::update_config::process_update_config(accounts, data)
        }
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
