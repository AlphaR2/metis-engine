use bytemuck::from_bytes;
use pinocchio::{
    AccountView, Address, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
};
use pinocchio_token::{
    instructions::{Burn, TransferChecked},
    state::Account,
};

use crate::{
    amm::state::{MARKET_DISCRIMINATOR, Market, MarketStatus},
    constants::{MARKET_SEED, MAX_OUTCOMES, OUTCOME_MINT_SEED, QUOTE_DECIMALS},
    error::MetisError,
    settlement::payout::compute_payout,
    validation,
};

/// Accounts:
///   0 market            (read)
///   1 vault             (writable)
///   2 holder            (signer)
///   3 holder_quote_ata  (writable, where the refund lands)
///   4 quote_mint
///   5 outcome_mint      (writable - either of the market's two outcome mints)
///   6 holder_ata        (writable, the shares being turned in)
///   7 token_program
///
/// Refund for a cancelled market. Holders of a cancelled market get the vault back in proportion
/// to their share count across BOTH sides - a share is a slice of everything, whichever outcome
/// it was for. Someone holding both outcomes calls this once per mint. It stays open while the
/// program is paused.
///
/// This is the simplest rule that is always solvent. It is not perfectly fair to someone who
/// paid more per share than someone else (early shares are cheaper), and that is a known,
/// deliberate trade for a rule that can never fail to pay.
pub fn process_claim_refund(accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    let [market, vault, holder, holder_quote_ata, quote_mint, outcome_mint, holder_ata, token_program] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(holder, MetisError::BettorMustSign)?;
    validation::require_writable(vault, MetisError::AccountNotWritable)?;
    validation::require_writable(holder_quote_ata, MetisError::AccountNotWritable)?;
    validation::require_writable(outcome_mint, MetisError::AccountNotWritable)?;
    validation::require_writable(holder_ata, MetisError::AccountNotWritable)?;
    validation::require_initialized_and_owned(market, &crate::ID, false, MetisError::InvalidMarketAccount)?;
    if token_program.address() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    let (pool, total_shares, market_bump, creator, nonce) = {
        let market_data = market.try_borrow()?;
        let m: &Market = from_bytes(&market_data);
        validation::require_discriminator(m.discriminator, MARKET_DISCRIMINATOR)?;
        if m.status != MarketStatus::Cancelled as u8 {
            return Err(MetisError::MarketNotCancelled.into());
        }
        validation::require_addresses_match(vault.address(), &m.vault, MetisError::WrongAccountReference)?;
        validation::require_addresses_match(quote_mint.address(), &m.quote_mint, MetisError::WrongAccountReference)?;
        let total = m.outcome_shares.iter().try_fold(0u64, |acc, &s| acc.checked_add(s)).ok_or(ProgramError::ArithmeticOverflow)?;
        (m.payout_pool, total, m.bump, m.creator, m.nonce)
    };

    // The mint must be one of this market's two outcome mints.
    let is_ours = (0..MAX_OUTCOMES).any(|i| {
        let index = [i as u8];
        Address::derive_program_address(&[OUTCOME_MINT_SEED, market.address().as_ref(), &index], &crate::ID)
            .is_some_and(|(pda, _)| &pda == outcome_mint.address())
    });
    if !is_ours {
        return Err(MetisError::InvalidPda.into());
    }

    let shares = {
        let ata = Account::from_account_view(holder_ata)?;
        validation::require_addresses_match(ata.mint(), outcome_mint.address(), MetisError::WrongAccountReference)?;
        validation::require_addresses_match(ata.owner(), holder.address(), MetisError::WrongAccountReference)?;
        ata.amount()
    };
    if shares == 0 {
        return Err(MetisError::AlreadyClaimed.into());
    }

    let refund = compute_payout(shares, pool, total_shares);

    // Burn first, pay second: a failure anywhere reverts the burn too.
    Burn::new(holder_ata, outcome_mint, holder, shares).invoke()?;

    if refund > 0 {
        let nonce_bytes = nonce.to_le_bytes();
        let bump_seed = [market_bump];
        let seeds = [
            Seed::from(MARKET_SEED),
            Seed::from(creator.as_ref()),
            Seed::from(nonce_bytes.as_ref()),
            Seed::from(bump_seed.as_ref()),
        ];
        TransferChecked::new(vault, quote_mint, holder_quote_ata, market, refund, QUOTE_DECIMALS)
            .invoke_signed(&[Signer::from(&seeds)])?;
    }
    Ok(())
}
