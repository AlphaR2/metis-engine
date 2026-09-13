use pinocchio::{AccountView, ProgramResult, error::ProgramError};
use wincode::Deserialize;

use crate::{
    errors::OracleError,
    state::{football_settled::Football, UpdateKind},
};

pub fn process_wincode_update_football(
    accounts: &mut [AccountView],
    update_kind: UpdateKind,
    data: &[u8],
) -> ProgramResult {
    let [creator, account_to_create, _system_program, _extra @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // signer checks
    if !creator.is_signer() {
        return Err(OracleError::CreatorMustSign.into());
    }
    // account ownership checks
    if !account_to_create.owned_by(&crate::ID) {
        return Err(OracleError::InvalidOraclePda.into());
    }

    // expected account state check
    if account_to_create.is_data_empty() {
        return Err(OracleError::OracleShouldNotBeEmpty.into());
    }

    // assert that data shape is indeed Football
    let football_length = Football::default().length();
    let football_data = data
        .get(..football_length)
        .ok_or(OracleError::InvalidAccountLength)?;

    let received_data_serialised =
        Football::deserialize(football_data).map_err(|_| OracleError::InvalidFootballData)?;

    // brutally wrie to data
    let borrowed_oracle_account_data = unsafe { account_to_create.borrow_unchecked_mut() };

    // let match_id_offset = 4;

    match update_kind {
        UpdateKind::Score => borrowed_oracle_account_data[4..5]
            .copy_from_slice(&received_data_serialised.score.to_le_bytes()),
        UpdateKind::Penalties => borrowed_oracle_account_data[5..6]
            .copy_from_slice(&received_data_serialised.penalties.to_le_bytes()),
        UpdateKind::EventState => {
            let status_bytes = wincode::serialize(&received_data_serialised.status)
                .expect("failed to serialize enum");
            borrowed_oracle_account_data[6..].copy_from_slice(&status_bytes);
        }
        _ => return Err(OracleError::InvalidUpdateKind.into()),
    }

    Ok(())
}
