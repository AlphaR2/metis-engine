use bytemuck::{CheckedBitPattern, NoUninit, Pod, Zeroable};
use pinocchio::{AccountView, ProgramResult, error::ProgramError};

use crate::{
    errors::OracleError,
    instructions::{authority::assert_oracle_updater, football::process_create_football},
    state::MarketKind,
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
        individual_authority @ ..,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // signer checks
    if !creator.is_signer() {
        return Err(OracleError::CreatorMustSign.into());
    }
    assert_oracle_updater(creator, individual_authority.first())?;
    // account ownership checks
    if !account_to_create.owned_by(_system_program.address()) {
        return Err(OracleError::OracleMustBeSystemOwned.into());
    }

    // expected account state check
    if !account_to_create.is_data_empty() {
        return Err(OracleError::OracleAlreadyInitialized.into());
    }

    let create_args: &[u8; CreateArgs::LEN] = data
        .get(..CreateArgs::LEN)
        .ok_or(ProgramError::InvalidInstructionData)?
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    let args = CreateArgs::from_bytes(&create_args);

    match args.market_kind {
        MarketKind::Football => process_create_football(accounts, &data[CreateArgs::LEN..])?,
        _ => return Err(ProgramError::InvalidInstructionData), // fix this later
    }
    Ok(())
}
