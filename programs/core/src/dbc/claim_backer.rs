use bytemuck::from_bytes_mut;
use pinocchio::{
    AccountView, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
};
use pinocchio_token::{
    instructions::{Burn, TransferChecked},
    state::{Account, Mint},
};

use crate::{
    amm::state::{MARKET_DISCRIMINATOR, Market, MarketStatus, SeedSource},
    constants::{MARKET_SEED, MAX_OUTCOMES, QUOTE_DECIMALS},
    error::MetisError,
    dbc::backer::{ata_address, outcome_mint_address, pro_rata},
    settlement::payout::compute_payout,
    validation,
};

/// Accounts:
///   0 market             (writable)
///   1 vault              (writable)
///   2 backer             (signer)
///   3 backer_quote_ata   (writable, where the payout lands)
///   4 quote_mint
///   5 backer_mint        (writable, the DBC token being turned in)
///   6 backer_token_ata   (writable, the backer's tokens - all of them are redeemed)
///   7 outcome_mint_0     (writable)
///   8 outcome_mint_1     (writable)
///   9 escrow_ata_0       (writable, the market's escrowed seed shares for outcome 0)
///   10 escrow_ata_1      (writable)
///   11 token_program
///
/// A backer redeems their slice of a DBC-backed market once it has settled or been cancelled. No
/// per-backer record exists: the slice is `tokens burned / backer tokens still outstanding`, taken
/// of what is left in the seed escrow and of what is left of the backers' revenue share. Each
/// redemption burns the tokens and the escrow's matching shares, so the next backer's ratio is
/// unchanged and every token is worth the same whenever it is redeemed.
///   - Settled: the slice of the escrow's WINNING shares is paid like any winning shares
///     (shares * payout pool / all winning shares), plus the slice of `backers_owed`.
///   - Cancelled: the slice of the escrow's shares on both sides is refunded like any other
///     holder's (shares * pool / all shares).
pub fn process_claim_backer(accounts: &mut [AccountView], _data: &[u8]) -> ProgramResult {
    let [
        market,
        vault,
        backer,
        backer_quote_ata,
        quote_mint,
        backer_mint,
        backer_token_ata,
        outcome_mint_0,
        outcome_mint_1,
        escrow_ata_0,
        escrow_ata_1,
        token_program,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(backer, MetisError::BettorMustSign)?;
    for writable in [
        &*market,
        &*vault,
        &*backer_quote_ata,
        &*backer_mint,
        &*backer_token_ata,
        &*outcome_mint_0,
        &*outcome_mint_1,
        &*escrow_ata_0,
        &*escrow_ata_1,
    ] {
        validation::require_writable(writable, MetisError::AccountNotWritable)?;
    }
    validation::require_initialized_and_owned(market, &crate::ID, true, MetisError::InvalidMarketAccount)?;
    if token_program.address() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    let outcome_mints = [&*outcome_mint_0, &*outcome_mint_1];
    let escrows = [&*escrow_ata_0, &*escrow_ata_1];

    // The tokens being redeemed, and how many are outstanding in all.
    let (tokens, supply) = {
        let ata = Account::from_account_view(backer_token_ata)?;
        validation::require_addresses_match(ata.mint(), backer_mint.address(), MetisError::BackerMintInvalid)?;
        validation::require_addresses_match(ata.owner(), backer.address(), MetisError::WrongAccountReference)?;
        (ata.amount(), Mint::from_account_view(backer_mint)?.supply())
    };
    if tokens == 0 {
        return Err(MetisError::NothingToRedeem.into());
    }

    // Everything read and decided from the market in one block; its slice of backers_owed is
    // taken here too, before any money moves, so a failure below reverts it.
    let market_addr = *market.address();
    let (payout, burn_from_escrow, market_bump, creator, nonce) = {
        let mut market_data = market.try_borrow_mut()?;
        let m: &mut Market = from_bytes_mut(&mut market_data);
        validation::require_discriminator(m.discriminator, MARKET_DISCRIMINATOR)?;
        if m.seed_source != SeedSource::DbcBackers as u8 {
            return Err(MetisError::NotBackerMarket.into());
        }
        validation::require_addresses_match(vault.address(), &m.vault, MetisError::WrongAccountReference)?;
        validation::require_addresses_match(quote_mint.address(), &m.quote_mint, MetisError::WrongAccountReference)?;
        validation::require_addresses_match(backer_mint.address(), &m.backer_mint, MetisError::BackerMintInvalid)?;
        for i in 0..MAX_OUTCOMES {
            validation::require_addresses_match(
                outcome_mints[i].address(),
                &outcome_mint_address(&market_addr, i as u8)?,
                MetisError::InvalidPda,
            )?;
            validation::require_addresses_match(
                escrows[i].address(),
                &ata_address(&market_addr, outcome_mints[i].address())?,
                MetisError::InvalidPda,
            )?;
        }
        let escrow_shares = [
            Account::from_account_view(escrow_ata_0)?.amount(),
            Account::from_account_view(escrow_ata_1)?.amount(),
        ];

        let (shares_paid, owed_slice, burn) = match MarketStatus::try_from(m.status)? {
            MarketStatus::Settled => {
                let w = m.winning_outcome as usize;
                let burn_w = pro_rata(escrow_shares[w], tokens, supply);
                let mut burn = [0u64; MAX_OUTCOMES];
                burn[w] = burn_w;
                let seed_payout = compute_payout(burn_w, m.payout_pool, m.outcome_shares[w]);
                let slice = pro_rata(m.backers_owed, tokens, supply);
                m.backers_owed -= slice;
                (seed_payout, slice, burn)
            }
            MarketStatus::Cancelled => {
                let burn = [pro_rata(escrow_shares[0], tokens, supply), pro_rata(escrow_shares[1], tokens, supply)];
                let total_shares = m.outcome_shares.iter().try_fold(0u64, |a, &s| a.checked_add(s)).ok_or(ProgramError::ArithmeticOverflow)?;
                let refund = compute_payout(burn[0] + burn[1], m.payout_pool, total_shares);
                (refund, 0, burn)
            }
            _ => return Err(MetisError::MarketNotSettled.into()),
        };
        (
            shares_paid.checked_add(owed_slice).ok_or(ProgramError::ArithmeticOverflow)?,
            burn,
            m.bump,
            m.creator,
            m.nonce,
        )
    };

    let nonce_bytes = nonce.to_le_bytes();
    let bump_seed = [market_bump];
    let market_seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(creator.as_ref()),
        Seed::from(nonce_bytes.as_ref()),
        Seed::from(bump_seed.as_ref()),
    ];

    // Burn first, pay second: a failure anywhere reverts the burns too, and burning is what makes
    // the redemption once-only.
    Burn::new(backer_token_ata, backer_mint, backer, tokens).invoke()?;
    for i in 0..MAX_OUTCOMES {
        if burn_from_escrow[i] > 0 {
            Burn::new(escrows[i], outcome_mints[i], market, burn_from_escrow[i])
                .invoke_signed(&[Signer::from(&market_seeds)])?;
        }
    }
    if payout > 0 {
        TransferChecked::new(vault, quote_mint, backer_quote_ata, market, payout, QUOTE_DECIMALS)
            .invoke_signed(&[Signer::from(&market_seeds)])?;
    }
    Ok(())
}
