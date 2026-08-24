#![allow(unexpected_cfgs)]
use pinocchio::{address::declare_id,  entrypoint, error::ProgramError, AccountView, Address, ProgramResult};

pub mod constants;
pub mod curve;
pub mod discriminator;
pub mod error;
pub mod instructions;
pub mod state;

#[cfg(test)]
mod tests;

declare_id!("2jt26iYKHghHDFVWQ8rnCdQc5B144Cos6HyJVw68qiv6");

entrypoint!(process_instruction);


pub fn process_instruction(
  program_id: &Address,
  accounts: &mut [AccountView],
  ix_data: &[u8],
)-> ProgramResult {

  assert_eq!(program_id, &ID, "program_id mismatch");

  let (discriminator, data) = ix_data.split_first_chunk::<8>().ok_or(ProgramError::InvalidInstructionData)?;

  match *discriminator {
    discriminator::INITIALIZE_MARKET => instructions::initialize_market::process_initialize_market(accounts, data),
    discriminator::PLACE_BET => instructions::place_bet::process_place_bet(accounts, data),
    discriminator::SETTLE_MARKET => instructions::settle_market::process_settle_market(accounts, data),
    discriminator::CLAIM => instructions::claim::process_claim(accounts, data),
    _ => Err(ProgramError::InvalidInstructionData),
  }
}