//  TODO: Oracle work will be added here to make sure this is complete settlement


use bytemuck::{from_bytes, from_bytes_mut, Pod, Zeroable};
use pinocchio::{
    error::ProgramError,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    AccountView, ProgramResult,
};

use crate::{
    amm::state::{Market, MarketStatus, MARKET_DISCRIMINATOR},
    error::OpenStakeError,
    settlement::payout::expected_vault_balance,
    settlement::state::{Vault, VAULT_DISCRIMINATOR},
    validation,
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct SettleMarketArgs {
    pub winning_outcome: u8,
    pub _padding: [u8; 7],
}

impl SettleMarketArgs {
    pub const LEN: usize = core::mem::size_of::<SettleMarketArgs>();

    pub fn load(bytes: &[u8]) -> Result<&Self, ProgramError> {
        if bytes.len() != Self::LEN {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(from_bytes::<Self>(bytes))
    }
}

const _: () = assert!(core::mem::size_of::<SettleMarketArgs>() == 8);


pub fn process_settle_market(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = SettleMarketArgs::load(data)?;

    let [market, vault, authority] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(authority)?;
    validation::require_writable(market)?;
    validation::require_initialized_and_owned(market, &crate::ID, false)?;
    validation::require_initialized_and_owned(vault, &crate::ID, false)?;

    
    let market_address = *market.address();

    let mut market_data = market.try_borrow_mut()?;
    let market_state: &mut Market = from_bytes_mut(&mut market_data);

    validation::require_discriminator(market_state.discriminator, MARKET_DISCRIMINATOR)?;

    // Only the market's own recorded authority can settle it.
    // TODO(single-point-of-failure): this single-signer check is the part
    // planned to be replaced by an oracle system
    // for now use authority

    validation::require_addresses_match(authority.address(), &market_state.authority)?;

    // Covers Uninitialized and already-Settled in one check.
    if market_state.status != MarketStatus::Open as u8 {
        return Err(OpenStakeError::InvalidStatusTransition.into());
    }
    if args.winning_outcome >= market_state.num_outcomes {
        return Err(OpenStakeError::InvalidOutcome.into());
    }

    // Betting must actually be closed before the market can settle
    let now = Clock::get()?.unix_timestamp;
    if now < market_state.bet_close_ts {
        return Err(OpenStakeError::MarketClosed.into());
    }

    // Vault solvency check, not an exact-balance check
    {
        let vault_data = vault.try_borrow()?;
        let vault_state: &Vault = from_bytes(&vault_data);
        validation::require_discriminator(vault_state.discriminator, VAULT_DISCRIMINATOR)?;
        validation::require_addresses_match(&vault_state.market, &market_address)?;

        let rent = Rent::get()?;
        let rent_minimum = rent.try_minimum_balance(core::mem::size_of::<Vault>())?;
        let expected = expected_vault_balance(rent_minimum, market_state.total_collected)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        if vault.lamports() < expected {
            return Err(OpenStakeError::VaultBalanceMismatch.into());
        }
    }

    market_state.winning_outcome = args.winning_outcome;
    market_state.status = MarketStatus::Settled as u8;

    Ok(())
}
