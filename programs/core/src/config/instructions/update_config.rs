use bytemuck::{from_bytes, from_bytes_mut, Pod, Zeroable};
use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use crate::{
    config::state::{Config, CONFIG_DISCRIMINATOR},
    validation,
};

// Same field set as InitializeConfigArgs, same "wholesale replace"

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct UpdateConfigArgs {
    pub market_init_fee: u64,
    pub fee_bps: u16,
    pub slippage_limit_bps: u16,
    pub max_in_bps_of_vault: u16,
    pub new_admin: Address,
    pub max_outcomes: u8,
    pub _padding: [u8; 1],
}

impl UpdateConfigArgs {
    pub const LEN: usize = core::mem::size_of::<UpdateConfigArgs>();

    pub fn load(bytes: &[u8]) -> Result<&Self, ProgramError> {
        if bytes.len() != Self::LEN {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(from_bytes::<Self>(bytes))
    }
}

const _: () = assert!(core::mem::size_of::<UpdateConfigArgs>() == 48);


pub fn process_update_config(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = UpdateConfigArgs::load(data)?;

    let [config, admin] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(admin)?;
    validation::require_initialized_and_owned(config, &crate::ID, true)?;

    let mut config_data = config.try_borrow_mut()?;
    let config_state: &mut Config = from_bytes_mut(&mut config_data);

    validation::require_discriminator(config_state.discriminator, CONFIG_DISCRIMINATOR)?;
    // Only the config's own recorded admin can update it — not the
    // bootstrap admin from initialize_config, which is never read again
    // once Config exists (see the note on constants::INITIAL_CONFIG_ADMIN).
    validation::require_addresses_match(admin.address(), &config_state.admin)?;

    if args.fee_bps as u32 > 10_000
        || args.slippage_limit_bps as u32 > 10_000
        || args.max_in_bps_of_vault as u32 > 10_000
    {
        return Err(ProgramError::InvalidInstructionData);
    }
    if args.new_admin == Address::default() {
        return Err(ProgramError::InvalidInstructionData);
    }

    config_state.market_init_fee = args.market_init_fee;
    config_state.fee_bps = args.fee_bps;
    config_state.slippage_limit_bps = args.slippage_limit_bps;
    config_state.max_in_bps_of_vault = args.max_in_bps_of_vault;
    config_state.admin = args.new_admin;
    config_state.max_outcomes = args.max_outcomes;

    Ok(())
}
