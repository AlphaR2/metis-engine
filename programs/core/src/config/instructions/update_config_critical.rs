use bytemuck::{Pod, Zeroable, from_bytes};
use pinocchio::{AccountView, Address, ProgramResult, error::ProgramError};

use crate::{
    config::state::{CONFIG_DISCRIMINATOR, Config},
    error::MetisError,
    validation,
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct UpdateConfigCriticalArgs {
    /// Only affects markets created after this call; live markets keep their own.
    pub quote_mint: Address,
    pub treasury: Address,
    /// Zero is allowed: it means DBC backing has no config to launch from.
    pub dbc_config: Address,
}

const _: () = assert!(core::mem::size_of::<UpdateConfigCriticalArgs>() == 96);

/// Accounts:
///   0 config      (writable)
///   1 first_root  (signer - a root signer)
///   2 second_root (signer - a different root signer)
///
/// The fields that decide where money goes or which token is accepted. Two distinct root
/// signatures, so one compromised key can't redirect the protocol's fees or swap the
/// stablecoin new markets are priced in.
pub fn process_update_config_critical(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    if data.len() != core::mem::size_of::<UpdateConfigCriticalArgs>() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let args: &UpdateConfigCriticalArgs = from_bytes(data);

    let [config, first_root, second_root] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_config_account(config, true)?;
    let mut config_data = config.try_borrow_mut()?;
    let config_state =
        validation::unpack_mut::<Config>(&mut config_data, MetisError::InvalidConfigAccount)?;
    validation::require_discriminator(config_state.discriminator, CONFIG_DISCRIMINATOR)?;

    validation::require_two_root_signers(&config_state.root, first_root, second_root)?;

    if args.quote_mint == Address::default() || args.treasury == Address::default() {
        return Err(MetisError::ConfigValueOutOfBounds.into());
    }

    config_state.quote_mint = args.quote_mint;
    config_state.treasury = args.treasury;
    config_state.dbc_config = args.dbc_config;

    Ok(())
}
