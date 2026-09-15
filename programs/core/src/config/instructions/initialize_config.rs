use bytemuck::{Pod, Zeroable, bytes_of, from_bytes};
use metis_authority::root::RootAuthority;
use pinocchio::{
    AccountView, Address, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, rent::Rent},
};
use pinocchio_system::instructions::CreateAccount;

use crate::{
    config::state::{CONFIG_DISCRIMINATOR, Config},
    constants::{
        CONFIG_SEED, GUARDIAN, MAX_FEE_PRIMARY_BPS, MAX_OUTCOMES,
        MAX_PRICE_IMPACT_BPS, MAX_VIRTUAL_LIQUIDITY_MULTIPLE, MIN_SEED, ROOT_SIGNERS,
    },
    error::MetisError,
    validation,
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct InitializeConfigArgs {
    pub market_init_fee: u64,
    pub virtual_liquidity_multiple: u64,
    pub fee_bps: u16,
    pub max_in_bps_of_vault: u16,
    pub quote_mint: Address,
    pub max_outcomes: u8,
    pub _padding: [u8; 3],
}

impl InitializeConfigArgs {
    pub const LEN: usize = core::mem::size_of::<InitializeConfigArgs>();

    pub fn load(bytes: &[u8]) -> Result<&Self, ProgramError> {
        if bytes.len() != Self::LEN {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(from_bytes::<Self>(bytes))
    }
}

const _: () = assert!(core::mem::size_of::<InitializeConfigArgs>() == 56);

pub fn process_initialize_config(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = InitializeConfigArgs::load(data)?;

    let [authority, system_program, config] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(authority, MetisError::AuthorityMustSign)?;
    validation::require_writable(authority, MetisError::AccountNotWritable)?;
    validation::require_writable(config, MetisError::AccountNotWritable)?;

    // No Config account exists yet to read root from
    let root = RootAuthority::new(&ROOT_SIGNERS, GUARDIAN, CONFIG_DISCRIMINATOR);
    if !root.is_root(*authority.address()) {
        return Err(MetisError::AuthorityMustSign.into());
    }

    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // Each bps field has its own real ceiling
    if args.fee_bps > MAX_FEE_PRIMARY_BPS
        || args.max_in_bps_of_vault > MAX_PRICE_IMPACT_BPS
        || args.virtual_liquidity_multiple > MAX_VIRTUAL_LIQUIDITY_MULTIPLE
    {
        return Err(ProgramError::InvalidInstructionData);
    }

    // The seed fee floor every impact/solvency figure in docs/research.md
    // is analysed against — config may raise it, never lower it.
    if args.market_init_fee < MIN_SEED {
        return Err(ProgramError::InvalidInstructionData);
    }

    // A config ceiling above what Market's account layout can hold would be
    // a live setting that can never actually be reached.
    if args.max_outcomes as usize > MAX_OUTCOMES {
        return Err(ProgramError::InvalidInstructionData);
    }

    // A zeroed quote mint would make every future initialize_market fail,
    // since markets must match Config.quote_mint and no real mint is zero.
    if args.quote_mint == Address::default() {
        return Err(ProgramError::InvalidInstructionData);
    }

    let config_seeds: [&[u8]; 2] = [CONFIG_SEED, GUARDIAN.as_ref()];

    let (config_pda, config_bump) = Address::derive_program_address(&config_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;

    validation::require_addresses_match(config.address(), &config_pda, MetisError::InvalidPda)?;

    let rent = Rent::get()?;
    let bump_seed = [config_bump];
    let signer_seeds = [
        Seed::from(CONFIG_SEED),
        Seed::from(GUARDIAN.as_ref()),
        Seed::from(bump_seed.as_ref()),
    ];
    CreateAccount {
        from: authority,
        to: config,
        lamports: rent.try_minimum_balance(core::mem::size_of::<Config>())?,
        space: core::mem::size_of::<Config>() as u64,
        owner: &crate::ID,
    }
    .invoke_signed(&[Signer::from(&signer_seeds)])?;

    let config_state = Config {
        market_init_fee: args.market_init_fee,
        fee_bps: args.fee_bps,
        max_in_bps_of_vault: args.max_in_bps_of_vault,
        virtual_liquidity_multiple: args.virtual_liquidity_multiple,
        root,
        quote_mint: args.quote_mint,
        discriminator: CONFIG_DISCRIMINATOR,
        max_outcomes: args.max_outcomes,
        bump: config_bump,
    };
    config
        .try_borrow_mut()?
        .copy_from_slice(bytes_of(&config_state));

    Ok(())
}
