use bytemuck::{bytes_of, from_bytes, Pod, Zeroable};
use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{rent::Rent, Sysvar},
    AccountView, Address, ProgramResult,
};
use pinocchio_system::instructions::{CreateAccount, Transfer};

use crate::constants::{MAX_OUTCOMES, POSITION_SEED};
use crate::curve::buy_shares_padded;
use crate::discriminator::{MARKET_DISCRIMINATOR, POSITION_DISCRIMINATOR};
use crate::error::OpenStakeError;
use crate::state::{Market, MarketStatus, Position};

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
    let place_args = PlaceBetArgs::load(data)?;

    if place_args.outcome_index as usize >= MAX_OUTCOMES {
        return Err(ProgramError::InvalidInstructionData);
    }

    if place_args.deposit_amount == 0 {
        return Err(OpenStakeError::PaymentTooSmall.into());
    }

    let [market, vault, position, bettor, system_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !bettor.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if !market.is_writable() || !vault.is_writable() || !position.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }
    if market.is_data_empty() || vault.is_data_empty() {
        return Err(ProgramError::UninitializedAccount);
    }
    if !market.owned_by(&crate::ID) || !vault.owned_by(&crate::ID) {
        return Err(ProgramError::IncorrectProgramId);
    }
    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // Read the market, check it, work out the new share numbers
    let (outcome_index, delta, new_outcome_shares, new_total_collected) = {
        let market_data = market.try_borrow()?;
        let market_state: &Market = from_bytes(&market_data);

        if market_state.discriminator != MARKET_DISCRIMINATOR {
            return Err(OpenStakeError::DiscriminatorMismatch.into());
        }
        if market_state.status != MarketStatus::Open as u8 {
            return Err(OpenStakeError::MarketNotOpen.into());
        }
        if &market_state.vault != vault.address() {
            return Err(ProgramError::InvalidAccountData);
        }

        let outcome_index = place_args.outcome_index as usize;
        if outcome_index >= market_state.num_outcomes as usize {
            return Err(OpenStakeError::InvalidOutcome.into());
        }

        let delta = buy_shares_padded(
            &market_state.outcome_shares,
            outcome_index,
            place_args.deposit_amount,
        )
        .ok_or(ProgramError::ArithmeticOverflow)?;

        let mut new_outcome_shares = market_state.outcome_shares;
        new_outcome_shares[outcome_index] += delta;
        let new_total_collected = market_state.total_collected + place_args.deposit_amount;

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
        let position_seeds: [&[u8]; 3] = [
            POSITION_SEED,
            market.address().as_ref(),
            bettor.address().as_ref(),
        ];
        let (position_pda, position_bump) =
            Address::derive_program_address(&position_seeds, &crate::ID)
                .ok_or(ProgramError::InvalidSeeds)?;
        if &position_pda != position.address() {
            return Err(ProgramError::InvalidSeeds);
        }

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
        if !position.owned_by(&crate::ID) {
            return Err(ProgramError::IncorrectProgramId);
        }

        let mut position_data = position.try_borrow_mut()?;
        let position_state: &mut Position = bytemuck::from_bytes_mut(&mut position_data);

        if position_state.discriminator != POSITION_DISCRIMINATOR {
            return Err(OpenStakeError::DiscriminatorMismatch.into());
        }
        if &position_state.market != market.address() || &position_state.owner != bettor.address() {
            return Err(ProgramError::InvalidAccountData);
        }

        position_state.outcome_shares[outcome_index] += delta;
    }

    // Move the real money in.
    Transfer {
        from: bettor,
        to: vault,
        lamports: place_args.deposit_amount,
    }
    .invoke()?;

    Ok(())
}
