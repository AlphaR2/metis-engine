use bytemuck::{bytes_of, from_bytes, Pod, Zeroable};
use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{clock::Clock, rent::Rent, Sysvar},
    AccountView, Address, ProgramResult,
};
use pinocchio_system::instructions::CreateAccount;

use crate::{
    config::state::{Config, CONFIG_DISCRIMINATOR},
    amm::state::{MARKET_DISCRIMINATOR, Market, MarketStatus, POSITION_DISCRIMINATOR, Position}, constants::{AUTHORITY, CONFIG_SEED, INITIAL_CONFIG_ADMIN, MARKET_SEED, OUTCOME_UNRESOLVED, POSITION_SEED, VAULT_SEED}, settlement::state::{VAULT_DISCRIMINATOR, Vault}, validation,
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct InitializeMarketArgs {
    pub nonce: u64,
    /// Unix timestamp (seconds) after which place_bet stops accepting bets
    /// for this market. Must be in the future at init.
    pub bet_close_ts: i64,
    pub num_outcomes: u8,
    pub _padding: [u8; 7],
}

impl InitializeMarketArgs {
    pub const LEN: usize = core::mem::size_of::<InitializeMarketArgs>();

    pub fn load(bytes: &[u8]) -> Result<&Self, ProgramError> {
        if bytes.len() != Self::LEN {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(from_bytes::<Self>(bytes))
    }
}

const _: () = assert!(core::mem::size_of::<InitializeMarketArgs>() == 24);

pub fn process_initialize_market(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = InitializeMarketArgs::load(data)?;

    let [market, vault, position, creator, system_program, config] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(creator)?;
    validation::require_writable(market)?;
    validation::require_writable(vault)?;
    validation::require_writable(position)?;

    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // load in config to get things started
    let config_seeds: [&[u8]; 2]  = [CONFIG_SEED, INITIAL_CONFIG_ADMIN.as_ref()];
    let (config_pda, _) = Address::derive_program_address(&config_seeds, &crate::ID).ok_or(ProgramError::InvalidSeeds)?;

    validation::require_addresses_match(config.address(), &config_pda)?;
    validation::require_initialized_and_owned(config, &crate::ID, false)?;

     let (market_init_fee, max_outcomes) = {
        let config_data = config.try_borrow()?;
        let config_state: &Config = from_bytes(&config_data);
        validation::require_discriminator(config_state.discriminator, CONFIG_DISCRIMINATOR)?;
        (config_state.market_init_fee, config_state.max_outcomes)
    };

    // check for num outcomes 
    if args.num_outcomes == 0
        || args.num_outcomes as usize > max_outcomes as usize
        || args.num_outcomes as usize > crate::constants::MAX_OUTCOMES
    {
        return Err(ProgramError::InvalidInstructionData);
    }

    // check for time and bet close time
    let now = Clock::get()?.unix_timestamp;
    if args.bet_close_ts <= now {
        return Err(ProgramError::InvalidInstructionData);
    }


    // Market PDA: [market, creator, nonce]
    let nonce_bytes = args.nonce.to_le_bytes();
    let market_seeds: [&[u8]; 3] = [MARKET_SEED, creator.address().as_ref(), &nonce_bytes];
    let (market_pda, market_bump) = Address::derive_program_address(&market_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    validation::require_addresses_match(market.address(), &market_pda)?;

    // Vault PDA: [vault, market]
    let vault_seeds: [&[u8]; 2] = [VAULT_SEED, market.address().as_ref()];
    let (vault_pda, vault_bump) = Address::derive_program_address(&vault_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    validation::require_addresses_match(vault.address(), &vault_pda)?;

    // Position PDA (creator's own): [position, market, creator]
    let position_seeds: [&[u8]; 3] = [POSITION_SEED, market.address().as_ref(), creator.address().as_ref()];
    let (position_pda, position_bump) = Address::derive_program_address(&position_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    validation::require_addresses_match(position.address(), &position_pda)?;

    let rent = Rent::get()?;
    let market_bump_seed = [market_bump];
    let vault_bump_seed = [vault_bump];
    let position_bump_seed = [position_bump];

    let market_signer_seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(creator.address().as_ref()),
        Seed::from(nonce_bytes.as_ref()),
        Seed::from(market_bump_seed.as_ref()),
    ];
    CreateAccount {
        from: creator,
        to: market,
        lamports: rent.try_minimum_balance(core::mem::size_of::<Market>())?,
        space: core::mem::size_of::<Market>() as u64,
        owner: &crate::ID,
    }
    .invoke_signed(&[Signer::from(&market_signer_seeds)])?;

    // Vault's balance covers rent plus the creator's fee — no separate
    // transfer needed, real money and rent live in one place.
    let vault_signer_seeds = [
        Seed::from(VAULT_SEED),
        Seed::from(market.address().as_ref()),
        Seed::from(vault_bump_seed.as_ref()),
    ];
    let vault_lamports = rent.try_minimum_balance(core::mem::size_of::<Vault>())? + market_init_fee;
    CreateAccount {
        from: creator,
        to: vault,
        lamports: vault_lamports,
        space: core::mem::size_of::<Vault>() as u64,
        owner: &crate::ID,
    }
    .invoke_signed(&[Signer::from(&vault_signer_seeds)])?;

    // The creator's own position — same as any bettor gets.
    let position_signer_seeds = [
        Seed::from(POSITION_SEED),
        Seed::from(market.address().as_ref()),
        Seed::from(creator.address().as_ref()),
        Seed::from(position_bump_seed.as_ref()),
    ];
    CreateAccount {
        from: creator,
        to: position,
        lamports: rent.try_minimum_balance(core::mem::size_of::<Position>())?,
        space: core::mem::size_of::<Position>() as u64,
        owner: &crate::ID,
    }
    .invoke_signed(&[Signer::from(&position_signer_seeds)])?;

    // Seed shares split evenly across every outcome so the market opens balanced.
    let seed_per_side = crate::amm::curve::symmetric_seed(market_init_fee, args.num_outcomes);
    let mut outcome_shares = [0u64; crate::constants::MAX_OUTCOMES];
    for i in 0..args.num_outcomes as usize {
        outcome_shares[i] = seed_per_side;
    }

    let market_state = Market {
        outcome_shares,
        total_collected: market_init_fee,
        bet_close_ts: args.bet_close_ts,
        creator: *creator.address(),
        authority: AUTHORITY,
        vault: *vault.address(),
        discriminator: MARKET_DISCRIMINATOR,
        num_outcomes: args.num_outcomes,
        status: MarketStatus::Open as u8,
        winning_outcome: OUTCOME_UNRESOLVED,
        bump: market_bump,
        _padding: [0u8; 4],
    };
    market.try_borrow_mut()?.copy_from_slice(bytes_of(&market_state));

    let vault_state = Vault {
        market: *market.address(),
        discriminator: VAULT_DISCRIMINATOR,
        bump: vault_bump,
    };
    vault.try_borrow_mut()?.copy_from_slice(bytes_of(&vault_state));

    let position_state = Position {
        outcome_shares,
        market: *market.address(),
        owner: *creator.address(),
        discriminator: POSITION_DISCRIMINATOR,
        claimed: 0,
        bump: position_bump,
        _padding: [0u8; 6],
    };
    position.try_borrow_mut()?.copy_from_slice(bytes_of(&position_state));

    Ok(())
}
