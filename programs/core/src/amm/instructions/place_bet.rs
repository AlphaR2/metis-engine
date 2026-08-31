use bytemuck::{bytes_of, from_bytes, Pod, Zeroable};
use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    AccountView, Address, ProgramResult,
};
use pinocchio_system::instructions::{CreateAccount, Transfer};

use crate::{
    config::state::{CONFIG_DISCRIMINATOR, Config},
    amm::curve::{buy_shares, cost},
    amm::state::{Market, MarketStatus, Position, MARKET_DISCRIMINATOR, POSITION_DISCRIMINATOR},
    constants::{MAX_OUTCOMES, POSITION_SEED, INITIAL_CONFIG_ADMIN, CONFIG_SEED},
    error::OpenStakeError,
    validation,
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct PlaceBetArgs {
    pub deposit_amount: u64,
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

const _: () = assert!(core::mem::size_of::<PlaceBetArgs>() == 16);

pub fn process_place_bet(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = PlaceBetArgs::load(data)?;

    if args.outcome_index as usize >= MAX_OUTCOMES {
        return Err(ProgramError::InvalidInstructionData);
    }
    if args.deposit_amount == 0 {
        return Err(OpenStakeError::PaymentTooSmall.into());
    }

    let [market, vault, position, bettor, system_program, config] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(bettor)?;
    validation::require_writable(bettor)?;
    validation::require_writable(market)?;
    validation::require_writable(vault)?;
    validation::require_writable(position)?;
    validation::require_initialized_and_owned(market, &crate::ID, false)?;
    validation::require_initialized_and_owned(vault, &crate::ID, false)?;
    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // load in config to get things started
    let config_seeds: [&[u8]; 2]  = [CONFIG_SEED, INITIAL_CONFIG_ADMIN.as_ref()];
    let (config_pda, _) = Address::derive_program_address(&config_seeds, &crate::ID).ok_or(ProgramError::InvalidSeeds)?;

    validation::require_addresses_match(config.address(), &config_pda)?;
    validation::require_initialized_and_owned(config, &crate::ID, false)?;

    // get slippage setup and protection params
    let (slippage_limit_bps, max_in_bps_of_vault) = {
        let config_data = config.try_borrow()?;
        let config_state: &Config = from_bytes(&config_data);
        validation::require_discriminator(config_state.discriminator, CONFIG_DISCRIMINATOR)?;
        (config_state.slippage_limit_bps, config_state.max_in_bps_of_vault)
    };

    // max_in: a single trade can never exceed this fraction of the vault's
    // CURRENT balance (before this deposit lands). 0 = uncapped.
    if max_in_bps_of_vault > 0 {
        let cap = (vault.lamports() as u128 * max_in_bps_of_vault as u128) / 10_000u128; //derive the cap as a value dependent on the money in vault. 
        if (args.deposit_amount as u128) > cap {
            return Err(OpenStakeError::SlippageExceeded.into());
        }
    }

    // Read the market, check it, work out the new share numbers.
    let (outcome_index, delta, new_outcome_shares, new_total_collected) = {
        let market_data = market.try_borrow()?;
        let market_state: &Market = from_bytes(&market_data);

        validation::require_discriminator(market_state.discriminator, MARKET_DISCRIMINATOR)?;
        if market_state.status != MarketStatus::Open as u8 {
            return Err(OpenStakeError::MarketNotOpen.into());
        }
        let now = Clock::get()?.unix_timestamp;
        if now >= market_state.bet_close_ts {
            return Err(OpenStakeError::MarketClosed.into());
        }
        validation::require_addresses_match(vault.address(), &market_state.vault)?;

        let outcome_index = args.outcome_index as usize;
        if outcome_index >= market_state.num_outcomes as usize {
            return Err(OpenStakeError::InvalidOutcome.into());
        }

        let old_total = market_state.outcome_shares[outcome_index];
        let cost_before = cost(&market_state.outcome_shares);
        let new_total = buy_shares(&market_state.outcome_shares, outcome_index, args.deposit_amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let delta = new_total.checked_sub(old_total).ok_or(ProgramError::ArithmeticOverflow)?;
        if delta == 0 {
            return Err(OpenStakeError::PaymentTooSmall.into());
        }

        // Global slippage protection, integer-only (no floats on-chain,
        // matching every other check in this program — isqrt/checked u128
        // math throughout curve.rs and payout.rs, no exception here).
        //
        // price_before = old_total / cost_before, so the naive flat-price
        // estimate "deposit / price_before" is the same as
        // "deposit * cost_before / old_total" — computed directly in u128,
        // no division into a float ever happens. 0 = uncapped.
        if slippage_limit_bps > 0 && old_total > 0 {
            let expected_shares = (args.deposit_amount as u128)
                .checked_mul(cost_before)
                .ok_or(ProgramError::ArithmeticOverflow)?
                / (old_total as u128);

            // shortfall = how many fewer shares the real trade minted vs
            // the naive estimate. None means the real trade did BETTER
            // than the naive estimate (can't happen on this convex curve,
            // but if it ever did, that's not a slippage violation either
            // way, so just skip the check).
            if let Some(shortfall) = expected_shares.checked_sub(delta as u128) {
                // slippage_pct = shortfall / expected_shares. Compare
                // against limit_bps/10_000 by cross-multiplying instead of
                // dividing, so this never touches a float or loses
                // precision: shortfall * 10_000 > limit_bps * expected.
                let lhs = shortfall.checked_mul(10_000).ok_or(ProgramError::ArithmeticOverflow)?;
                let rhs = expected_shares
                    .checked_mul(slippage_limit_bps as u128)
                    .ok_or(ProgramError::ArithmeticOverflow)?;
                if lhs > rhs {
                    return Err(OpenStakeError::SlippageExceeded.into());
                }
            }
        }

        let mut new_outcome_shares = market_state.outcome_shares;
        new_outcome_shares[outcome_index] = new_outcome_shares[outcome_index]
            .checked_add(delta)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let new_total_collected = market_state
            .total_collected
            .checked_add(args.deposit_amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        (outcome_index, delta, new_outcome_shares, new_total_collected)
    };

    // Write the updated market back.
    {
        let mut market_data = market.try_borrow_mut()?;
        let market_state: &mut Market = bytemuck::from_bytes_mut(&mut market_data);
        market_state.outcome_shares = new_outcome_shares;
        market_state.total_collected = new_total_collected;
    }

    // Position: top up if it already exists, create it if this is the
    // bettor's first bet in this market.
    if position.is_data_empty() {
        let position_seeds: [&[u8]; 3] = [POSITION_SEED, market.address().as_ref(), bettor.address().as_ref()];
        let (position_pda, position_bump) = Address::derive_program_address(&position_seeds, &crate::ID)
            .ok_or(ProgramError::InvalidSeeds)?;
        validation::require_addresses_match(position.address(), &position_pda)?;

        let rent = Rent::get()?;
        let bump_seed = [position_bump];
        let signer_seeds = [
            Seed::from(POSITION_SEED),
            Seed::from(market.address().as_ref()),
            Seed::from(bettor.address().as_ref()),
            Seed::from(bump_seed.as_ref()),
        ];
        CreateAccount {
            from: bettor,
            to: position,
            lamports: rent.try_minimum_balance(core::mem::size_of::<Position>())?,
            space: core::mem::size_of::<Position>() as u64,
            owner: &crate::ID,
        }
        .invoke_signed(&[Signer::from(&signer_seeds)])?;

        let mut fresh_shares = [0u64; MAX_OUTCOMES];
        fresh_shares[outcome_index] = delta;

        let position_state = Position {
            outcome_shares: fresh_shares,
            market: *market.address(),
            owner: *bettor.address(),
            discriminator: POSITION_DISCRIMINATOR,
            claimed: 0,
            bump: position_bump,
            _padding: [0u8; 6],
        };
        position.try_borrow_mut()?.copy_from_slice(bytes_of(&position_state));
    } else {
        validation::require_owned_by(position, &crate::ID)?;

        let mut position_data = position.try_borrow_mut()?;
        let position_state: &mut Position = bytemuck::from_bytes_mut(&mut position_data);

        validation::require_discriminator(position_state.discriminator, POSITION_DISCRIMINATOR)?;
        validation::require_addresses_match(&position_state.market, market.address())?;
        validation::require_addresses_match(&position_state.owner, bettor.address())?;

        position_state.outcome_shares[outcome_index] = position_state.outcome_shares[outcome_index]
            .checked_add(delta)
            .ok_or(ProgramError::ArithmeticOverflow)?;
    }

    // Move the real money in.
    Transfer {
        from: bettor,
        to: vault,
        lamports: args.deposit_amount,
    }
    .invoke()?;

    Ok(())
}
