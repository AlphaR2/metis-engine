use bytemuck::{Pod, Zeroable, from_bytes};
use pinocchio::{
    AccountView, Address, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock},
};
use pinocchio_associated_token_account::instructions::CreateIdempotent;
use pinocchio_token::instructions::{MintTo, TransferChecked};

use crate::{
    amm::curve::{cost, priced_shares, quote_buy},
    amm::quote::fee_of,
    amm::state::{MARKET_DISCRIMINATOR, Market, MarketStatus},
    config::state::{CONFIG_DISCRIMINATOR, Config},
    constants::{
        MARKET_SEED, MAX_OUTCOMES, MAX_PRICE_IMPACT_BPS,
        OUTCOME_MINT_SEED, QUOTE_DECIMALS,
    },
    error::MetisError,
    validation,
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct PlaceBetArgs {
    pub deposit_amount: u64,
    /// Caller-supplied (computed off-chain from the quote
    /// the trader was actually shown), not a program constant. The whole
    /// job of this check is `if they recieve a small share than the min share out they set 
    /// (delta < min_shares_out { reject })`. This is an
    /// r-agnostic guard against a stale quote, never a formula artifact.
    pub min_shares_out: u64,
    pub outcome_index: u8,
    pub _padding: [u8; 7],
}

impl PlaceBetArgs {
    pub const LEN: usize = core::mem::size_of::<PlaceBetArgs>();

    pub fn load(bytes: &[u8]) -> Result<&Self, ProgramError> {
        if bytes.len() != Self::LEN {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(from_bytes::<Self>(bytes))
    }
}

const _: () = assert!(core::mem::size_of::<PlaceBetArgs>() == 24);

/// Accounts:
///   0  market             (writable)
///   1  vault              (writable, the market's USDC account)
///   2  bettor             (signer, writable)
///   3  bettor_quote_ata   (writable, the USDC they're paying from)
///   4  quote_mint         (the USDC mint)
///   5  outcome_mint       (writable, only the outcome being bought)
///   6  bettor_outcome_ata (writable, where their new shares land)
///   7  config
///   8  system_program
///   9  token_program
///   10 ata_program
///
/// Unlike initialize_market this list is fixed regardless of MAX_OUTCOMES - a
/// bet only ever touches the one outcome being bought.
pub fn process_place_bet(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = PlaceBetArgs::load(data)?;

    // Cheap rejections first, before we touch any account.
    if args.outcome_index as usize >= MAX_OUTCOMES {
        return Err(ProgramError::InvalidInstructionData);
    }
    if args.deposit_amount == 0 {
        return Err(MetisError::PaymentTooSmall.into());
    }

    let [
        market,
        vault,
        bettor,
        bettor_quote_ata,
        quote_mint,
        outcome_mint,
        bettor_outcome_ata,
        config,
        system_program,
        token_program,
        ata_program,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // The bettor signs and pays; everything they touch changes.
    validation::require_signer(bettor, MetisError::BettorMustSign)?;
    validation::require_writable(bettor, MetisError::AccountNotWritable)?;
    validation::require_writable(market, MetisError::AccountNotWritable)?;
    validation::require_writable(vault, MetisError::AccountNotWritable)?;
    validation::require_writable(bettor_quote_ata, MetisError::AccountNotWritable)?;
    // The mint's supply grows and the bettor's account gets credited.
    validation::require_writable(outcome_mint, MetisError::AccountNotWritable)?;
    validation::require_writable(bettor_outcome_ata, MetisError::AccountNotWritable)?;
    validation::require_initialized_and_owned(
        market,
        &crate::ID,
        false,
        MetisError::InvalidMarketAccount,
    )?;

    // Make sure each "program" account really is the program we think it is,
    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    if token_program.address() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    if ata_program.address() != &pinocchio_associated_token_account::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    validation::require_config_account(config, false)?;
    validation::require_not_paused(config)?;

    // get trade-cap protection params
    let max_in_bps_of_vault = {
        let config_data = config.try_borrow()?;
        let config_state =
            validation::unpack::<Config>(&config_data, MetisError::InvalidConfigAccount)?;
        validation::require_discriminator(config_state.discriminator, CONFIG_DISCRIMINATOR)?;
        config_state.dials.max_in_bps_of_vault
    };

    // Read the market, run the curve, and work out everything we need before
    // touching any account. The borrow is dropped at the end of this block so
    // we can write back to the market afterwards.
    let (delta, new_outcome_shares, new_total_collected, new_fees_accrued, market_bump, creator, nonce) = {
        let market_data = market.try_borrow()?;
        let market_state: &Market = from_bytes(&market_data);

        validation::require_discriminator(market_state.discriminator, MARKET_DISCRIMINATOR)?;
        if market_state.status != MarketStatus::Open as u8 {
            return Err(MetisError::MarketNotOpen.into());
        }

        // Betting stops well before the event starts. After this point the AMM
        // never mints again - the supply is fixed and any further trading
        // happens between people on the secondary market.

        let now = Clock::get()?.unix_timestamp;
        if now >= market_state.primary_end {
            return Err(MetisError::MarketClosed.into());
        }

        // The vault and mint passed in must be the ones this market recorded,
        // or a bettor could pay into one market and be credited in another.
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

        let outcome_index = args.outcome_index as usize;
        if outcome_index >= market_state.num_outcomes as usize {
            return Err(MetisError::InvalidOutcome.into());
        }

        // Priced view: real shares plus this market's frozen virtual offset
        // (docs/mvp.md 1.4). Every curve call below uses this, never
        // market_state.outcome_shares directly — that field stays read-only,
        // which is what the write-back and payouts read.
        // basically call `priced_shares` function and feed 
        // it the market_state.outcome_shares and market_state.virtual_per_side
        // So it returns  the num of shares [yes+v, no+v].
        let priced = priced_shares(&market_state.outcome_shares, market_state.virtual_per_side)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        // The fee comes off the deposit BEFORE the curve sees it, using the rate this market was
        // created with (not today's Config). Only the net amount buys shares and enters the pot;
        // the fee sits in the vault beside the pot and is swept at settlement.
        let fee = fee_of(args.deposit_amount, market_state.fee_bps);
        let net = args.deposit_amount.checked_sub(fee).ok_or(ProgramError::ArithmeticOverflow)?;
        if net == 0 {
            return Err(MetisError::PaymentTooSmall.into());
        }

        // Trade-size cap, measured against priced depth rather than raw
        // vault balance (docs/mvp.md 1.2/1.6) — scales with the market's
        // true curve depth, real + virtual, not just dollars sitting in the
        // vault. Early on this is what stops a whale swinging the whole
        // market in one shot. Config guarantees the cap is never 0 (off).
        {
            let depth = cost(&priced);
            // get the max amount allowed to be deposited in the market based on the max_in_bps_of_vault
            // already it is expected that this is calculated 
            let cap = (depth * max_in_bps_of_vault as u128) / 10_000u128;
            if (net as u128) > cap {
                return Err(MetisError::TradeCapExceeded.into());
            }
        }

        // The curve, executed once: gives the real share delta, the
        // execution price, and the resulting price impact off the same
        // transition — no separate re-derivation for each check below.
        let quote = quote_buy(&priced, outcome_index, net).ok_or(MetisError::PaymentTooSmall)?;

        let delta = quote.shares_received;

        // docs/mvp.md 1.5: caller-supplied, r-agnostic guard against a
        // stale quote. The user share protection parameter. 
        if delta < args.min_shares_out {
            return Err(MetisError::SlippageExceeded.into());
        }

        // docs/mvp.md 1.2: forward-only hard impact ceiling.
        // check that the price impact is not too high 
        // (MAX_PRICE_IMPACT_BPS is 10pp), if it is, reject the trade.

        if quote.price_impact_bps > MAX_PRICE_IMPACT_BPS as u32 {
            return Err(MetisError::PriceImpactExceeded.into());
        }

        let mut new_outcome_shares = market_state.outcome_shares;
        new_outcome_shares[outcome_index] = new_outcome_shares[outcome_index]
            .checked_add(delta)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let new_total_collected = market_state
            .total_collected
            .checked_add(net)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let new_fees_accrued = market_state
            .fees_accrued
            .checked_add(fee)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        (
            delta,
            new_outcome_shares,
            new_total_collected,
            new_fees_accrued,
            market_state.bump,
            market_state.creator,
            market_state.nonce,
        )
    };

    // The mint must be this market's mint for this outcome. Without this a
    // bettor could pay here and have shares minted on some other market.
    let index_byte = [args.outcome_index];
    let mint_seeds: [&[u8]; 3] = [OUTCOME_MINT_SEED, market.address().as_ref(), &index_byte];
    let (mint_pda, _) = Address::derive_program_address(&mint_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    validation::require_addresses_match(outcome_mint.address(), &mint_pda, MetisError::InvalidPda)?;

    // Write the updated market back.
    {
        let mut market_data = market.try_borrow_mut()?;
        let market_state: &mut Market = bytemuck::from_bytes_mut(&mut market_data);
        market_state.outcome_shares = new_outcome_shares;
        market_state.total_collected = new_total_collected;
        market_state.fees_accrued = new_fees_accrued;
    }

    // Money in: the gross deposit. What lands in the vault is always total_collected + fees_accrued
    // - the pot plus the fees - which is exactly what settlement's solvency check relies on.

    TransferChecked::new(
        bettor_quote_ata,
        quote_mint,
        vault,
        bettor,
        args.deposit_amount,
        QUOTE_DECIMALS,
    )
    .invoke()?;

    // The bettor needs somewhere to hold these shares. "Idempotent" means it quietly does nothing if they already have an account for this outcome, which is the normal case for anyone betting more than once.
    CreateIdempotent {
        funding_account: bettor,
        account: bettor_outcome_ata,
        wallet: bettor,
        mint: outcome_mint,
        system_program,
        token_program,
    }
    .invoke()?;

    // Shares out. The market PDA is the mint authority, so it signs by handing
    // over the seeds it was built from

    let nonce_bytes = nonce.to_le_bytes();
    let market_bump_seed = [market_bump];
    let market_signer_seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(creator.as_ref()),
        Seed::from(nonce_bytes.as_ref()),
        Seed::from(market_bump_seed.as_ref()),
    ];

    MintTo::new(outcome_mint, bettor_outcome_ata, market, delta)
        .invoke_signed(&[Signer::from(&market_signer_seeds)])?;

    Ok(())
}
