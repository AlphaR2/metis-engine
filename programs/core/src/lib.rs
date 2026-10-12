#![allow(unexpected_cfgs)]
use pinocchio::{
    AccountView, Address, ProgramResult, address::declare_id, error::ProgramError,
};

pub mod amm;
pub mod clob;
pub mod config;
pub mod dbc;
pub mod constants;
pub mod error;
pub mod settlement;
pub mod validation;

#[cfg(test)]
mod tests;

declare_id!("79uea7X34YHRUQFkLAR5rwupHQf6Z7ASbBGfzxRDu8ws");

// Off when another crate links this one as a library (the aggregator reads layouts and runs
// the curve math off-chain); two entrypoints in one binary would collide.
#[cfg(not(feature = "no-entrypoint"))]
pinocchio::entrypoint!(process_instruction);

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
        amm::state::SET_METADATA => amm::instructions::set_metadata::process_set_metadata(accounts, data),
        amm::state::PLACE_BET => amm::instructions::place_bet::process_place_bet(accounts, data),
        amm::state::CLOSE_PRIMARY => amm::instructions::close_primary::process_close_primary(accounts, data),
        amm::state::INITIALIZE_BACKED_MARKET => {
            dbc::initialize_backed_market::process_initialize_backed_market(accounts, data)
        }
        amm::state::ACTIVATE_MARKET => {
            dbc::activate_market::process_activate_market(accounts, data)
        }
        settlement::state::CLAIM_BACKER => {
            dbc::claim_backer::process_claim_backer(accounts, data)
        }
        settlement::state::CANCEL_PENDING_MARKET => {
            dbc::cancel_pending_market::process_cancel_pending_market(accounts, data)
        }
        settlement::state::SETTLE_MARKET => {
            settlement::instructions::settle_market::process_settle_market(accounts, data)
        }
        settlement::state::SWEEP_FEES => settlement::instructions::sweep_fees::process_sweep_fees(accounts, data),
        settlement::state::CLAIM => settlement::instructions::claim::process_claim(accounts, data),
        settlement::state::CANCEL_MARKET => {
            settlement::instructions::cancel_market::process_cancel_market(accounts, data)
        }
        settlement::state::CLAIM_REFUND => {
            settlement::instructions::claim_refund::process_claim_refund(accounts, data)
        }
        settlement::state::CLOSE_MARKET => {
            settlement::instructions::close_market::process_close_market(accounts, data)
        }
        settlement::state::FORFEIT_CREATOR_FEES => {
            settlement::instructions::forfeit_fees::process_forfeit_creator_fees(accounts, data)
        }
        config::state::INITIALIZE_CONFIG => {
            config::instructions::initialize_config::process_initialize_config(accounts, data)
        }
        config::state::UPDATE_CONFIG => {
            config::instructions::update_config::process_update_config(accounts, data)
        }
        config::state::UPDATE_CONFIG_CRITICAL => {
            config::instructions::update_config_critical::process_update_config_critical(accounts, data)
        }
        config::state::PAUSE => config::instructions::pause::process_pause(accounts),
        config::state::UNPAUSE => config::instructions::pause::process_unpause(accounts),
        config::state::ATTEST_AUTHORITY => {
            config::instructions::attest_authority::process_attest_authority(accounts)
        }
        config::state::REMOVE_AUTHORITY => {
            config::instructions::remove_authority::process_remove_authority(accounts)
        }
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
