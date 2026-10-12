use bytemuck::from_bytes_mut;
use pinocchio::{
    AccountView, ProgramResult,
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock},
};

use crate::{
    amm::state::{MARKET_DISCRIMINATOR, Market, MarketStatus},
    error::MetisError,
    validation,
};

/// Accounts:
///   0 market      (writable)
///   1 config
///   2 signer      (an operator or root at any time; anyone once the raise deadline has passed)
///   3 operator_authority (optional - the signer's attestation, needed unless the signer is a root)
///
/// Gives up on a DBC-backed market whose raise never opened it. Nothing is held in the market at
/// that point (the vault is empty, no seed shares exist), so there is nothing to refund here:
/// backers still hold their tokens and sell them back on the DBC curve, which is how the raise's
/// own mechanism returns their money. The market becomes Cancelled so it can be closed.
pub fn process_cancel_pending_market(accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    let [market, config, signer, rest @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if rest.len() > 1 {
        return Err(ProgramError::InvalidInstructionData);
    }
    validation::require_writable(market, MetisError::AccountNotWritable)?;
    validation::require_initialized_and_owned(market, &crate::ID, true, MetisError::InvalidMarketAccount)?;
    validation::require_config_account(config, false)?;

    let mut market_data = market.try_borrow_mut()?;
    let m: &mut Market = from_bytes_mut(&mut market_data);
    validation::require_discriminator(m.discriminator, MARKET_DISCRIMINATOR)?;
    if m.status != MarketStatus::Pending as u8 {
        return Err(MetisError::InvalidStatusTransition.into());
    }

    let now = Clock::get()?.unix_timestamp;
    if now < m.raise_deadline {
        // Before the deadline only the protocol's own keys may give up (the raise failed early).
        validation::require_operator(signer, rest.first(), config).map_err(|_| ProgramError::from(MetisError::RaiseNotExpired))?;
    }

    m.payout_pool = 0;
    m.status = MarketStatus::Cancelled as u8;
    m.resolved_at = now;
    Ok(())
}
