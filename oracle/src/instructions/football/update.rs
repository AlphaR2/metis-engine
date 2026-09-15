use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{
    AccountView, ProgramResult,
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock},
};
use shank::ShankType;

use crate::{
    constants::FOOTBALL_SETTLED_DISCRIMINATOR,
    error::OracleError,
    instructions::football::{CFootballMatchPhase, FootballSettled},
    state::UpdateKind,
    validation::{parse_prefix, require_discriminator, unpack_mut},
};

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy, ShankType)]
pub struct UpdateFootballArgs {
    pub score: u8,
    pub penalties: u8,
    pub status: CFootballMatchPhase,
}

impl UpdateFootballArgs {
    pub const LEN: usize = core::mem::size_of::<Self>();
    pub fn from_bytes(data: &[u8; Self::LEN]) -> &Self {
        bytemuck::checked::from_bytes::<Self>(data)
    }
}

pub fn process_update_football(
    accounts: &mut [AccountView],
    update_kind: UpdateKind,
    data: &[u8],
) -> ProgramResult {
    // creator's signature/authority was already checked by process_update_oracle. 
    let [_creator, account_to_create, _system_program, _extra @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let update_args = parse_prefix::<{ UpdateFootballArgs::LEN }>(data)?;
    let update_data = UpdateFootballArgs::from_bytes(update_args);

    let borrowed_oracle_account_data = unpack_mut::<FootballSettled>(
        unsafe { account_to_create.borrow_unchecked_mut() },
        OracleError::InvalidFootballData,
    )?;
    require_discriminator(
        borrowed_oracle_account_data.discriminator,
        FOOTBALL_SETTLED_DISCRIMINATOR,
    )?;

    if update_kind == UpdateKind::Score {
        borrowed_oracle_account_data.score = update_data.score;
    } else if update_kind == UpdateKind::Penalties {
        borrowed_oracle_account_data.penalties = update_data.penalties;
    } else if update_kind == UpdateKind::EventState {
        borrowed_oracle_account_data.set_match_status(update_data.status)?;
    }

    let time = Clock::get()?;
    let current_time = time.unix_timestamp as u64;

    borrowed_oracle_account_data.updated_at_unix = current_time.to_le_bytes();

    Ok(())
}
