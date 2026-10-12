use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{AccountView, ProgramResult, error::ProgramError};

use crate::{
    error::OracleError,
    instructions::{authority::assert_oracle_updater, football::process_create_football},
    state::MarketKind,
    validation::{
        load_root_authority, parse_prefix, require_empty, require_not_paused, require_owned_by,
        require_signer,
    },
};

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy)]
pub struct CreateArgs {
    pub market_kind: MarketKind,
}
impl CreateArgs {
    pub const LEN: usize = core::mem::size_of::<Self>();
    pub fn from_bytes(data: &[u8; Self::LEN]) -> &Self {
        bytemuck::checked::from_bytes::<Self>(data)
    }
}

pub fn process_create_oracle(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // load accounts
    let [
        creator,
        account_to_create,
        _system_program,
        authority,
        individual_authority @ ..,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // Only ever one optional trailing account (creator's own individual authority, when
    // creator isn't root) - reject anything beyond that instead of silently ignoring it.
    if individual_authority.len() > 1 {
        return Err(ProgramError::InvalidInstructionData);
    }

    require_signer(creator, OracleError::CreatorMustSign)?;

    // Read the actual on-chain RootAuthority account rather than trusting a compiled-in
    // constant - this is what lets root rotation and the pause flag apply here.
    let authority_state = load_root_authority(authority)?;
    require_not_paused(authority_state)?;

    assert_oracle_updater(creator, authority_state, individual_authority.first())?;
    require_owned_by(
        account_to_create,
        &pinocchio_system::ID,
        OracleError::OracleMustBeSystemOwned,
    )?;
    require_empty(account_to_create, OracleError::OracleAlreadyInitialized)?;

    let create_args = parse_prefix::<{ CreateArgs::LEN }>(data)?;
    let args = CreateArgs::from_bytes(create_args);

    match args.market_kind {
        MarketKind::Football => process_create_football(accounts, &data[CreateArgs::LEN..])?,
        //TODO: Add for other market kinds as they are implemented
        _ => return Err(ProgramError::InvalidInstructionData), // fix this later
    }
    Ok(())
}
