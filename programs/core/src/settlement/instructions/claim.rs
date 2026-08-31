use bytemuck::from_bytes;
use pinocchio::{error::ProgramError, AccountView, ProgramResult};

use crate::{
    amm::state::{Market, MarketStatus, Position, MARKET_DISCRIMINATOR, POSITION_DISCRIMINATOR},
    error::OpenStakeError,
    settlement::payout::compute_payout,
    settlement::state::{Vault, VAULT_DISCRIMINATOR},
    validation,
};

pub fn process_claim(accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {

    let [market, vault, position, bettor] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(bettor)?;
    // bettor is credited (payout + rent back) — needs
    // to be writable same as vault and position.
    validation::require_writable(vault)?;
    validation::require_writable(position)?;
    validation::require_writable(bettor)?;
    validation::require_initialized_and_owned(market, &crate::ID, false)?;
    validation::require_initialized_and_owned(vault, &crate::ID, false)?;
    validation::require_initialized_and_owned(position, &crate::ID, false)?;

    // Read the market once: must be Settled, and this is where
    // winning_outcome, total_collected, and the winning side's total
    // share count all come from.
    let (winning_outcome, total_collected, all_winning_shares) = {
        let market_data = market.try_borrow()?;
        let market_state: &Market = from_bytes(&market_data);

        validation::require_discriminator(market_state.discriminator, MARKET_DISCRIMINATOR)?;
        if market_state.status != MarketStatus::Settled as u8 {
            return Err(OpenStakeError::MarketNotSettled.into());
        }
        validation::require_addresses_match(vault.address(), &market_state.vault)?;

        let winning_outcome = market_state.winning_outcome as usize;
        (
            winning_outcome,
            market_state.total_collected,
            market_state.outcome_shares[winning_outcome],
        )
    };

    // Vault: check its own state only. The exact-balance sanity check lives
    // in settle_market instead — it's only ever true before any payouts,
    // and every claim after the first legitimately drains the vault below
    // that number.
    {
        let vault_data = vault.try_borrow()?;
        let vault_state: &Vault = from_bytes(&vault_data);
        validation::require_discriminator(vault_state.discriminator, VAULT_DISCRIMINATOR)?;
        validation::require_addresses_match(&vault_state.market, market.address())?;
    }

    // Read the position: must belong to this bettor and this market, and
    // can't have been claimed already.
    let winning_shares = {
        let position_data = position.try_borrow()?;
        let position_state: &Position = from_bytes(&position_data);

        validation::require_discriminator(position_state.discriminator, POSITION_DISCRIMINATOR)?;
        validation::require_addresses_match(&position_state.market, market.address())?;
        validation::require_addresses_match(&position_state.owner, bettor.address())?;
        if position_state.claimed != 0 {
            return Err(OpenStakeError::AlreadyClaimed.into());
        }

        position_state.outcome_shares[winning_outcome]
    };

    let payout = compute_payout(winning_shares, total_collected, all_winning_shares);

    // Mark claimed before moving money — never pay the same position twice.
    {
        let mut position_data = position.try_borrow_mut()?;
        let position_state: &mut Position = bytemuck::from_bytes_mut(&mut position_data);
        position_state.claimed = 1; // mark claimed
    }

    // Pay out of the vault. Both vault and position are owned by this
    // program, so lamports move directly — no CPI needed.
    if payout > 0 {
        let vault_lamports = vault.lamports().checked_sub(payout).ok_or(ProgramError::InsufficientFunds)?;
        vault.set_lamports(vault_lamports);
        let bettor_lamports = bettor.lamports().checked_add(payout).ok_or(ProgramError::ArithmeticOverflow)?;
        bettor.set_lamports(bettor_lamports);
    }

    // Close the position: wipe the data before draining lamports. 
    {
        let mut position_data = position.try_borrow_mut()?;
        position_data.fill(0);
    }
    let position_rent = position.lamports();
    position.set_lamports(0);
    let bettor_lamports = bettor.lamports().checked_add(position_rent).ok_or(ProgramError::ArithmeticOverflow)?;
    bettor.set_lamports(bettor_lamports);

    Ok(())
}
