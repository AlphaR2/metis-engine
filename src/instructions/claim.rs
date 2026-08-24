use bytemuck::from_bytes;
use pinocchio::{error::ProgramError, AccountView, ProgramResult};

use crate::discriminator::{MARKET_DISCRIMINATOR, POSITION_DISCRIMINATOR, VAULT_DISCRIMINATOR};
use crate::error::OpenStakeError;
use crate::state::{Market, MarketStatus, Position, Vault};

// No extra data needed — every account this touches is already in the
// accounts list, so there's nothing else to pass in.
//
// # Accounts
//
// | # | Account   | Writable | Signer |
// |---|-----------|----------|--------|
// | 0 | market    | no       | no     |
// | 1 | vault     | yes      | no     |
// | 2 | position  | yes      | no     |
// | 3 | bettor    | yes      | yes    |
pub fn process_claim(accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    let [market, vault, position, bettor] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !bettor.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    // bettor is credited lamports twice below (payout + rent back) —
    // needs to be writable same as vault and position.
    if !vault.is_writable() || !position.is_writable() || !bettor.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }
    if market.is_data_empty() || vault.is_data_empty() || position.is_data_empty() {
        return Err(ProgramError::UninitializedAccount);
    }
    if !market.owned_by(&crate::ID) || !vault.owned_by(&crate::ID) || !position.owned_by(&crate::ID) {
        return Err(ProgramError::IncorrectProgramId);
    }

    // Read the market once: must be Settled, and this is where
    // winning_outcome, total_collected, and the winning side's total
    // share count all come from.
    let (winning_outcome, total_collected, all_winning_shares) = {
        let market_data = market.try_borrow()?;
        let market_state: &Market = from_bytes(&market_data);

        if market_state.discriminator != MARKET_DISCRIMINATOR {
            return Err(OpenStakeError::DiscriminatorMismatch.into());
        }
        if market_state.status != MarketStatus::Settled as u8 {
            return Err(OpenStakeError::MarketNotSettled.into());
        }
        if &market_state.vault != vault.address() {
            return Err(ProgramError::InvalidAccountData);
        }

        let winning_outcome = market_state.winning_outcome as usize;
        (
            winning_outcome,
            market_state.total_collected,
            market_state.outcome_shares[winning_outcome],
        )
    };

    // Vault: check its own state only. The exact-balance sanity check
    // (vault lamports == rent_minimum + total_collected) belongs in
    // settle_market instead, not here — it's only ever true before any
    // payouts have happened. Every claim after the first legitimately
    // drains the vault below that number, since total_collected is fixed
    // but vault.lamports() shrinks with each payout. Checking it here
    // would reject every claim after the first one on every market.
    {
        let vault_data = vault.try_borrow()?;
        let vault_state: &Vault = from_bytes(&vault_data);
        if vault_state.discriminator != VAULT_DISCRIMINATOR {
            return Err(OpenStakeError::DiscriminatorMismatch.into());
        }
        if &vault_state.market != market.address() {
            return Err(ProgramError::InvalidAccountData);
        }
    }

    // Read the position: must belong to this bettor and this market,
    // and can't have been claimed already.
    let winning_shares = {
        let position_data = position.try_borrow()?;
        let position_state: &Position = from_bytes(&position_data);

        if position_state.discriminator != POSITION_DISCRIMINATOR {
            return Err(OpenStakeError::DiscriminatorMismatch.into());
        }
        if &position_state.market != market.address() || &position_state.owner != bettor.address() {
            return Err(ProgramError::InvalidAccountData);
        }
        if position_state.claimed != 0 {
            return Err(OpenStakeError::AlreadyClaimed.into());
        }

        position_state.outcome_shares[winning_outcome]
    };

    // payout = my winning shares * total collected / all winning shares.
    // Zero if this position didn't hold the winning side — still fine to
    // close out and reclaim rent either way. u128 so the multiply can't
    // overflow; floor division rounds toward the pool, same rule as
    // everywhere else in curve.rs.
    let payout: u64 = if all_winning_shares == 0 || winning_shares == 0 {
        0
    } else {
        ((winning_shares as u128) * (total_collected as u128) / (all_winning_shares as u128)) as u64
    };

    // Mark claimed before moving money — never pay the same position twice.
    {
        let mut position_data = position.try_borrow_mut()?;
        let position_state: &mut Position = bytemuck::from_bytes_mut(&mut position_data);
        position_state.claimed = 1;
    }

    // Pay out of the vault. Both vault and position are owned by this
    // program, so lamports move directly — no CPI needed, a program can
    // debit lamports from any account it owns.
    if payout > 0 {
        let vault_lamports = vault.lamports().checked_sub(payout).ok_or(ProgramError::InsufficientFunds)?;
        vault.set_lamports(vault_lamports);
        let bettor_lamports = bettor.lamports().checked_add(payout).ok_or(ProgramError::ArithmeticOverflow)?;
        bettor.set_lamports(bettor_lamports);
    }

    // Close the position: draining it to zero lamports is enough — the
    // runtime removes any account that ends a transaction at zero.
    let position_rent = position.lamports();
    position.set_lamports(0);
    let bettor_lamports = bettor.lamports().checked_add(position_rent).ok_or(ProgramError::ArithmeticOverflow)?;
    bettor.set_lamports(bettor_lamports);

    Ok(())
}
