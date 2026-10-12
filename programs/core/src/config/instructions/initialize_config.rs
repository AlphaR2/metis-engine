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
    config::state::{CONFIG_DISCRIMINATOR, Config, ConfigDials},
    constants::{CONFIG_SEED, GUARDIAN, ROOT_SIGNERS},
    error::MetisError,
    validation,
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct InitializeConfigArgs {
    pub dials: ConfigDials,
    pub quote_mint: Address,
    pub treasury: Address,
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

const _: () = assert!(core::mem::size_of::<InitializeConfigArgs>() == 104);

/// Accounts:
///   0 payer       (signer, writable - a root signer, pays rent)
///   1 second_root (signer - a different root signer)
///   2 system_program
///   3 config      (writable, PDA [config, GUARDIAN])
///
/// Two root signers, not one: this is the first write of quote_mint and treasury, the same
/// fields update_config_critical guards with two signatures. With one, whichever single key
/// called first would decide where every fee is paid.
pub fn process_initialize_config(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = InitializeConfigArgs::load(data)?;

    let [payer, second_root, system_program, config] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_writable(payer, MetisError::AccountNotWritable)?;
    validation::require_writable(config, MetisError::AccountNotWritable)?;

    // No Config account exists yet to read root from, so it comes from the hardcoded keys.
    let root = RootAuthority::new(&ROOT_SIGNERS, GUARDIAN, CONFIG_DISCRIMINATOR);
    validation::require_two_root_signers(&root, payer, second_root)?;

    if system_program.address() != &pinocchio_system::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    args.dials.validate()?;
    // A zeroed address would make every market fail (no real mint is zero) or burn the
    // protocol's fees into an account nobody controls.
    if args.quote_mint == Address::default() || args.treasury == Address::default() {
        return Err(MetisError::ConfigValueOutOfBounds.into());
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
        from: payer,
        to: config,
        lamports: rent.try_minimum_balance(core::mem::size_of::<Config>())?,
        space: core::mem::size_of::<Config>() as u64,
        owner: &crate::ID,
    }
    .invoke_signed(&[Signer::from(&signer_seeds)])?;

    let config_state = Config {
        dials: args.dials,
        root,
        quote_mint: args.quote_mint,
        treasury: args.treasury,
        dbc_config: Address::default(),
        discriminator: CONFIG_DISCRIMINATOR,
        bump: config_bump,
        _padding: [0; 5],
    };
    config
        .try_borrow_mut()?
        .copy_from_slice(bytes_of(&config_state));

    Ok(())
}
