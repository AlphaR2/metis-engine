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
    constants::{MARKET_SEED, OUTCOME_MINT_SEED, QUOTE_DECIMALS},
    error::MetisError,
    settlement::payout::compute_payout,
    validation,
};

/// Accounts:
///   0 market              (writable is not needed - only read)
///   1 vault               (writable, the market's USDC account)
///   2 bettor              (signer)
///   3 bettor_quote_ata    (writable, where the payout lands)
///   4 quote_mint          (the USDC mint)
///   5 winning_mint        (writable, the mint of the outcome that won)
///   6 bettor_winning_ata  (writable, the shares being cashed in)
///   7 token_program
///
/// Cashes in winning shares. The shares are burned and the holder gets their
/// slice of the pool. There's no separate "losing claim" here: losing tokens
/// are simply worthless, and their holder can close the account themselves to
/// get the rent back.
pub fn process_claim(accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    let [
        market,
        vault,
        bettor,
        bettor_quote_ata,
        quote_mint,
        winning_mint,
        bettor_winning_ata,
        token_program,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(bettor, MetisError::BettorMustSign)?;
    validation::require_writable(vault, MetisError::AccountNotWritable)?;
    validation::require_writable(bettor_quote_ata, MetisError::AccountNotWritable)?;
    // Burning reduces both the holder's balance and the mint's supply.
    validation::require_writable(winning_mint, MetisError::AccountNotWritable)?;
    validation::require_writable(bettor_winning_ata, MetisError::AccountNotWritable)?;
    validation::require_initialized_and_owned(
        market,
        &crate::ID,
        false,
        MetisError::InvalidMarketAccount,
    )?;

    if token_program.address() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // Read everything we need from the market in one go, then drop the borrow.
    let (winning_outcome, total_collected, all_winning_shares, market_bump, creator, nonce) = {
        let market_data = market.try_borrow()?;
        let market_state: &Market = from_bytes(&market_data);

        validation::require_discriminator(market_state.discriminator, MARKET_DISCRIMINATOR)?;
        if market_state.status != MarketStatus::Settled as u8 {
            return Err(MetisError::MarketNotSettled.into());
        }

        validation::require_addresses_match(
            vault.address(),
            &market_state.vault,
            MetisError::WrongAccountReference,
        )?;
        validation::require_addresses_match(
            quote_mint.address(),
            &market_state.quote_mint,
            MetisError::WrongAccountReference,
        )?;

        let winning_outcome = market_state.winning_outcome as usize;

        (
            winning_outcome,
            market_state.total_collected,
            // The winning side's total, frozen when the market settled.
            //
            // This is the number every payout divides by, and it MUST come
            // from here rather than from the mint's live supply. Each claim
            // burns tokens, so the supply shrinks as people cash in - dividing
            // by it would hand later claimers a bigger and bigger share until
            // the vault ran dry.
            market_state.outcome_shares[winning_outcome],
            market_state.bump,
            market_state.creator,
            market_state.nonce,
        )
    };

    // The mint must be this market's mint for the winning outcome. Otherwise
    // someone could burn a worthless losing token and get paid for it.
    let index_byte = [winning_outcome as u8];
    let mint_seeds: [&[u8]; 3] = [OUTCOME_MINT_SEED, market.address().as_ref(), &index_byte];
    let (mint_pda, _) = Address::derive_program_address(&mint_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    validation::require_addresses_match(winning_mint.address(), &mint_pda, MetisError::InvalidPda)?;

    // How many winning shares this person holds. Their whole balance is cashed
    // in at once - there's no reason to leave any behind, since after
    // settlement the token has no future value beyond this payout.
    let winning_shares = {
        let ata = Account::from_account_view(bettor_winning_ata)?;
        validation::require_addresses_match(
            ata.mint(),
            winning_mint.address(),
            MetisError::WrongAccountReference,
        )?;
        validation::require_addresses_match(
            ata.owner(),
            bettor.address(),
            MetisError::WrongAccountReference,
        )?;
        ata.amount()
    };

    if winning_shares == 0 {
        return Err(MetisError::AlreadyClaimed.into());
    }

    let payout = compute_payout(winning_shares, total_collected, all_winning_shares);

    // Burn first, pay second. Burning is what makes claiming once-only: the
    // tokens are gone, so a repeat call finds a zero balance and stops above.
    // Doing it in this order means a failure anywhere reverts the burn too.
    Burn::new(bettor_winning_ata, winning_mint, bettor, winning_shares).invoke()?;

    if payout > 0 {
        // The market PDA owns the vault, so it authorises the transfer out by
        // handing over the seeds it was built from.
        let nonce_bytes = nonce.to_le_bytes();
        let market_bump_seed = [market_bump];
        let market_signer_seeds = [
            Seed::from(MARKET_SEED),
            Seed::from(creator.as_ref()),
            Seed::from(nonce_bytes.as_ref()),
            Seed::from(market_bump_seed.as_ref()),
        ];

        TransferChecked::new(
            vault,
            quote_mint,
            bettor_quote_ata,
            market,
            payout,
            QUOTE_DECIMALS,
        )
        .invoke_signed(&[Signer::from(&market_signer_seeds)])?;
    }

    Ok(())
}
