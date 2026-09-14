use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{AccountView, ProgramResult, error::ProgramError};

use crate::{
    error::OracleError,
    instructions::{authority::assert_oracle_updater, football::process_update_football},
    state::UpdateKind,
    validation::{parse_prefix, require_not_empty, require_owned_by, require_signer},
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

    require_signer(creator, OracleError::CreatorMustSign)?;
    assert_oracle_updater(creator, individual_authority.first())?;
    require_owned_by(account_to_create, &crate::ID, OracleError::OracleMustBeSystemOwned)?;
    require_not_empty(account_to_create, OracleError::OracleShouldNotBeEmpty)?;

    let update_args = parse_prefix::<{ UpdateArgs::LEN }>(data)?;
    let args = UpdateArgs::from_bytes(update_args);

    match args.update_kind {
        UpdateKind::Score | UpdateKind::Penalties | UpdateKind::EventState => {
            let update_data = &data[UpdateArgs::LEN..];
            process_update_football(accounts, args.update_kind, update_data)?
        }
        _ => return Err(OracleError::InvalidUpdateKind.into()),
    }
    Ok(())
}
