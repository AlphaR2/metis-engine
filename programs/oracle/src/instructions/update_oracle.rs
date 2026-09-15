use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{AccountView, ProgramResult, error::ProgramError};

use crate::{
    constants::{AUTHORITY_SEED, GUARDIAN, ORACLE_AUTHORITY_DISCRIMINATOR},
    error::OracleError,
    instructions::{authority::assert_oracle_updater, football::process_update_football},
    state::{RootAuthority, UpdateKind},
    validation::{
        derive_and_check_pda, parse_prefix, require_discriminator, require_not_empty,
        require_owned_by, require_signer, unpack,
    },
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
    // constant - this is what lets root rotation (and the pause flag) actually apply here,
    // the same as it already does for attest/remove/close_oracle.
    derive_and_check_pda(
        &[AUTHORITY_SEED, GUARDIAN.as_ref()],
        &crate::ID,
        authority.address(),
        OracleError::InvalidAuthorityPda,
    )?;
    require_owned_by(authority, &crate::ID, OracleError::InvalidAuthorityPda)?;
    let authority_state = unpack::<RootAuthority>(
        unsafe { authority.borrow_unchecked() },
        OracleError::InvalidAuthority,
    )?;
    require_discriminator(authority_state.discriminator, ORACLE_AUTHORITY_DISCRIMINATOR)?;

    assert_oracle_updater(creator, authority_state, individual_authority.first())?;
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
