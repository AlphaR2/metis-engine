use bytemuck::{Pod, Zeroable, from_bytes};
use pinocchio::{AccountView, Address, ProgramResult, error::ProgramError};

use crate::{
    config::state::{CONFIG_DISCRIMINATOR, Config},
    constants::{
        MAX_FEE_PRIMARY_BPS, MAX_OUTCOMES, MAX_PRICE_IMPACT_BPS, MAX_VIRTUAL_LIQUIDITY_MULTIPLE,
        MIN_SEED,
    },
    error::MetisError,
    validation,
};

// Same field set as InitializeConfigArgs, same "wholesale replace" - root itself is not
// rotatable through this instruction (same as oracle: no root-rotation instruction exists
// there either). Rotating root signers is deliberately separate, higher-friction work, not a
// field any single admin could once unilaterally overwrite.

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct UpdateConfigArgs {
    pub market_init_fee: u64,
    /// Only affects markets created after this call — see Config's own
    /// doc comment on this field.
    pub virtual_liquidity_multiple: u64,
    pub fee_bps: u16,
    pub max_in_bps_of_vault: u16,
    /// Only affects markets created after this call.
    pub quote_mint: Address,
    pub max_outcomes: u8,
    pub _padding: [u8; 3],
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

const _: () = assert!(core::mem::size_of::<UpdateConfigArgs>() == 56);

pub fn process_update_config(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let args = UpdateConfigArgs::load(data)?;

    let [config, admin] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    validation::require_signer(admin, MetisError::AuthorityMustSign)?;
    validation::require_initialized_and_owned(
        config,
        &crate::ID,
        true,
        MetisError::InvalidConfigAccount,
    )?;

    let mut config_data = config.try_borrow_mut()?;
    let config_state = validation::unpack_mut::<Config>(&mut config_data, MetisError::InvalidConfigAccount)?;

    validation::require_discriminator(config_state.discriminator, CONFIG_DISCRIMINATOR)?;
    // Any current root signer can update config parameters - same as oracle's root check.
    if !config_state.root.is_root(*admin.address()) {
        return Err(MetisError::AuthorityMustSign.into());
    }

    // Same real per-field ceilings as initialize_config
    if args.fee_bps > MAX_FEE_PRIMARY_BPS
        || args.max_in_bps_of_vault > MAX_PRICE_IMPACT_BPS
        || args.virtual_liquidity_multiple > MAX_VIRTUAL_LIQUIDITY_MULTIPLE
    {
        return Err(ProgramError::InvalidInstructionData);
    }

    // The seed fee floor every impact/solvency figure in docs/research.md
    // is analysed against MIN_SEED, so we enforce it here too.
    if args.market_init_fee < MIN_SEED {
        return Err(ProgramError::InvalidInstructionData);
    }

    // A config ceiling above what Market's account layout can hold would be
    // a live setting that can never actually be reached.
    if args.max_outcomes as usize > MAX_OUTCOMES {
        return Err(ProgramError::InvalidInstructionData);
    }

    // Same reasoning as before: a zeroed quote mint would silently break
    if args.quote_mint == Address::default() {
        return Err(ProgramError::InvalidInstructionData);
    }

    config_state.market_init_fee = args.market_init_fee;
    config_state.fee_bps = args.fee_bps;
    config_state.max_in_bps_of_vault = args.max_in_bps_of_vault;
    config_state.virtual_liquidity_multiple = args.virtual_liquidity_multiple;
    config_state.quote_mint = args.quote_mint;
    config_state.max_outcomes = args.max_outcomes;

    Ok(())
}
