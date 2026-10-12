use bytemuck::from_bytes;
use pinocchio::{AccountView, ProgramResult, error::ProgramError};

use crate::{
    config::state::{CONFIG_DISCRIMINATOR, Config, ConfigDials},
    error::MetisError,
    validation,
};

// Replaces the business dials wholesale, within their hardcoded bounds. One root signer is
// enough because no dial here can move money anywhere new - the fields that can (treasury,
// quote_mint, dbc_config) live behind update_config_critical, and the root set itself can't be
// changed by any config instruction at all.

/// Accounts:
///   0 config (writable)
///   1 admin  (signer - any current root signer)
pub fn process_update_config(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    if data.len() != core::mem::size_of::<ConfigDials>() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let dials: &ConfigDials = from_bytes(data);

    let [config, admin] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(admin, MetisError::AuthorityMustSign)?;
    validation::require_config_account(config, true)?;
    let mut config_data = config.try_borrow_mut()?;
    let config_state =
        validation::unpack_mut::<Config>(&mut config_data, MetisError::InvalidConfigAccount)?;
    validation::require_discriminator(config_state.discriminator, CONFIG_DISCRIMINATOR)?;

    if !config_state.root.is_root(*admin.address()) {
        return Err(MetisError::UnauthorizedAuthority.into());
    }

    dials.validate()?;
    config_state.dials = *dials;

    Ok(())
}
