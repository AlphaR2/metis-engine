use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{AccountView, ProgramResult, error::ProgramError};

use crate::{
    errors::OracleError,
    instructions::{
        process_wincode_create_football, process_wincode_update_football, CreateArgs,
    },
    state::{MarketKind, UpdateKind},
};

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy)]
struct WincodeUpdateArgs {
    market_kind: MarketKind,
    update_kind: UpdateKind,
}

impl WincodeUpdateArgs {
    const LEN: usize = core::mem::size_of::<Self>();

    fn from_bytes(data: &[u8; Self::LEN]) -> &Self {
        bytemuck::checked::from_bytes::<Self>(data)
    }
}

pub fn process_wincode_create_oracle(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let create_args: &[u8; CreateArgs::LEN] = data
        .get(..CreateArgs::LEN)
        .ok_or(ProgramError::InvalidInstructionData)?
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    let args = CreateArgs::from_bytes(create_args);

    match args.market_kind {
        MarketKind::Football => process_wincode_create_football(accounts, &data[CreateArgs::LEN..]),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

pub fn process_wincode_update_oracle(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let update_args: &[u8; WincodeUpdateArgs::LEN] = data
        .get(..WincodeUpdateArgs::LEN)
        .ok_or(ProgramError::InvalidInstructionData)?
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    let args = WincodeUpdateArgs::from_bytes(update_args);

    match args.market_kind {
        MarketKind::Football => match args.update_kind {
            UpdateKind::Score | UpdateKind::Penalties | UpdateKind::EventState => {
                process_wincode_update_football(
                    accounts,
                    args.update_kind,
                    &data[WincodeUpdateArgs::LEN..],
                )
            }
            _ => Err(OracleError::InvalidUpdateKind.into()),
        },
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
