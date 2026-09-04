use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{AccountView, ProgramResult, error::ProgramError};

use crate::{
    errors::OracleError,
    instructions::{authority::assert_oracle_updater, football::process_update_football},
    state::UpdateKind,
};

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy)]
pub struct UpdateArgs {
    pub update_kind: UpdateKind,
}
impl UpdateArgs {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn from_bytes(data: &[u8; Self::LEN]) -> &Self {
        bytemuck::checked::from_bytes::<Self>(data)
    }
}

pub fn process_update_oracle(accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
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
    if !account_to_create.owned_by(&crate::ID) {
        return Err(OracleError::OracleMustBeSystemOwned.into());
    }

    // expected account state check
    if account_to_create.is_data_empty() {
        return Err(OracleError::OracleShouldNotBeEmpty.into());
    }

    let update_args: &[u8; UpdateArgs::LEN] = data
        .get(..UpdateArgs::LEN)
        .ok_or(ProgramError::InvalidInstructionData)?
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    let args = UpdateArgs::from_bytes(update_args);

    // we already confirmed that it's not empty
    // let stored_market_kind = unsafe { account_to_create.borrow_unchecked() }[0];

    // if it's not empty and we checked that the account was created with this program, then this check isn't super necessary
    // if stored_market_kind != MarketKind::Football as u8 {
    //     return Err(ProgramError::InvalidInstructionData);
    // }

    match args.update_kind {
        UpdateKind::Score | UpdateKind::Penalties | UpdateKind::EventState => {
            let update_data = &data[UpdateArgs::LEN..];
            process_update_football(accounts, args.update_kind, update_data)?
        }
        _ => return Err(OracleError::InvalidUpdateKind.into()),
    }
    Ok(())
}
