use bytemuck::{bytes_of, from_bytes, Pod, Zeroable};
use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{rent::Rent, Sysvar},
    AccountView, Address, ProgramResult,
};
use pinocchio_system::instructions::CreateAccount;

use crate::{
    config::state::{Config, CONFIG_DISCRIMINATOR},
    constants::{CONFIG_SEED, INITIAL_CONFIG_ADMIN},
    validation,
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct InitializeConfigArgs {
    pub market_init_fee: u64,
    pub fee_bps: u16,
    pub slippage_limit_bps: u16,
    pub max_in_bps_of_vault: u16,
    pub admin: Address,
    pub max_outcomes: u8,
    pub _padding: [u8; 1],
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

const _: () = assert!(core::mem::size_of::<InitializeConfigArgs>() == 48);


pub fn process_initialize_config(
    accounts: &mut [AccountView], 
    data: &[u8]) 
    -> ProgramResult {
    let args = InitializeConfigArgs::load(data)?;

    let [authority, system_program, config] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(authority)?;
    validation::require_writable(authority)?;
    validation::require_writable(config)?;

    validation::require_addresses_match(authority.address(), &INITIAL_CONFIG_ADMIN)?;

    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    if args.fee_bps as u32 > 10_000
        || args.slippage_limit_bps as u32 > 10_000
        || args.max_in_bps_of_vault as u32 > 10_000
    {
        return Err(ProgramError::InvalidInstructionData);
    }

    let config_seeds: [&[u8]; 2] = [CONFIG_SEED, authority.address().as_ref()];

    let (config_pda, config_bump) = Address::derive_program_address(&config_seeds, &crate::ID)
        .ok_or(ProgramError::InvalidSeeds)?;

    validation::require_addresses_match(config.address(), &config_pda)?;

    let rent = Rent::get()?;
    let bump_seed = [config_bump];
    let signer_seeds = [
        Seed::from(CONFIG_SEED),
        Seed::from(authority.address().as_ref()),
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
        slippage_limit_bps: args.slippage_limit_bps,
        max_in_bps_of_vault: args.max_in_bps_of_vault,
        admin: args.admin,
        discriminator: CONFIG_DISCRIMINATOR,
        max_outcomes: args.max_outcomes,
        bump: config_bump,
    };
    config.try_borrow_mut()?.copy_from_slice(bytes_of(&config_state));

    Ok(())
}
