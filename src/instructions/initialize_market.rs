use bytemuck::{bytes_of, from_bytes, Pod, Zeroable};
use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{rent::Rent, Sysvar},
    AccountView, Address, ProgramResult,
};
use pinocchio_system::instructions::CreateAccount;

use crate::{
    constants::{AUTHORITY, MARKET_INIT_FEE, MARKET_SEED, OUTCOME_UNRESOLVED, POSITION_SEED, VAULT_SEED},
    curve::symmetric_seed,
    discriminator::{MARKET_DISCRIMINATOR, POSITION_DISCRIMINATOR, VAULT_DISCRIMINATOR},
    state::{Market, MarketStatus, Position, Vault},
};

// Args coming in from the client.

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct InitializeMarketArgs {
    pub nonce: u64,
    pub num_outcomes: u8,
    pub _padding: [u8; 7],
}

impl InitializeMarketArgs {
    // Calculate the size of the struct in bytes.
    pub const LEN: usize = core::mem::size_of::<InitializeMarketArgs>();

    //direct byte copy from the input bytes to the struct, returning a reference to the struct.
    pub fn load(bytes: &[u8]) -> Result<&Self, ProgramError> {
        if bytes.len() != Self::LEN {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(from_bytes::<Self>(bytes))
    }
}

//length assertion
const _: () = assert!(core::mem::size_of::<InitializeMarketArgs>() == 16);


/// Process the initialize market instruction. 
pub fn process_initialize_market(
    accounts: &mut [AccountView], 
    data: &[u8]
) -> ProgramResult {
    //get the args from the instruction data
    let args = InitializeMarketArgs::load(data)?;

    // outcome count must be non-zero and within the allowed range, error if more than that. 
    if args.num_outcomes == 0 || args.num_outcomes as usize > crate::constants::MAX_OUTCOMES {
        return Err(ProgramError::InvalidInstructionData);
    }

    // unload all the accounts that partake from this exercise, error if not enough accounts are provided.
    let [market, vault, position, creator, system_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // runtime checks

    if !creator.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if !market.is_writable() || !vault.is_writable() || !position.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }
    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // Market PDA: [market, creator, nonce]
    let nonce_bytes = args.nonce.to_le_bytes();
    let market_seeds: [&[u8]; 3] = [MARKET_SEED, creator.address().as_ref(), &nonce_bytes];

    //derive market pda
    let (market_pda, market_bump) = Address::derive_program_address(&market_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    if &market_pda != market.address() {
        return Err(ProgramError::InvalidSeeds);
    }

    // Vault PDA: [vault, market]
    let vault_seeds: [&[u8]; 2] = [VAULT_SEED, market.address().as_ref()];
    //derive vault pda for market 
    let (vault_pda, vault_bump) = Address::derive_program_address(&vault_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    if &vault_pda != vault.address() {
        return Err(ProgramError::InvalidSeeds);
    }

    // Position PDA (creator's own): [position, market, creator]
    let position_seeds: [&[u8]; 3] = [POSITION_SEED, market.address().as_ref(), creator.address().as_ref()];
    //derive position pda for creator's own position
    let (position_pda, position_bump) = Address::derive_program_address(&position_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;
    if &position_pda != position.address() {
        return Err(ProgramError::InvalidSeeds);
    }

    // extra params needed for account creation and initialization
    let rent = Rent::get()?; // rent to know what to pay before account creation
    let market_bump_seed = [market_bump];
    let vault_bump_seed = [vault_bump];
    let position_bump_seed = [position_bump];

    // Create the market account.
    let market_signer_seeds = [
        Seed::from(MARKET_SEED),
        Seed::from(creator.address().as_ref()),
        Seed::from(nonce_bytes.as_ref()),
        Seed::from(market_bump_seed.as_ref()),
    ];
    //create the market account with the required lamports and space, and set the owner to the program id
    CreateAccount {
        from: creator,
        to: market,
        lamports: rent.try_minimum_balance(core::mem::size_of::<Market>())?,
        space: core::mem::size_of::<Market>() as u64,
        owner: &crate::ID,
    }
    .invoke_signed(&[Signer::from(&market_signer_seeds)])?;

    // Create the vault. Its balance covers rent plus the creator's fee —
    // no separate transfer needed, real money and rent live in one place.
    let vault_signer_seeds = [
        Seed::from(VAULT_SEED),
        Seed::from(market.address().as_ref()),
        Seed::from(vault_bump_seed.as_ref()),
    ];
    let vault_lamports = rent.try_minimum_balance(core::mem::size_of::<Vault>())? + MARKET_INIT_FEE;
    CreateAccount {
        from: creator,
        to: vault,
        lamports: vault_lamports,
        space: core::mem::size_of::<Vault>() as u64,
        owner: &crate::ID,
    }
    .invoke_signed(&[Signer::from(&vault_signer_seeds)])?;

    // Create the creator's own position — same as any bettor gets.
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

    // The initial purchase sharing for creator as creator needs to be the first buyer. This derives hisSeed shares, split evenly across every outcome so the market opens balanced.
    let seed_per_side = symmetric_seed(MARKET_INIT_FEE, args.num_outcomes);
    let mut outcome_shares = [0u64; crate::constants::MAX_OUTCOMES];
    for i in 0..args.num_outcomes as usize {
        outcome_shares[i] = seed_per_side;
    }

    let market_state = Market {
        outcome_shares,
        total_collected: MARKET_INIT_FEE,
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
