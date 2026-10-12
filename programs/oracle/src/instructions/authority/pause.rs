use metis_authority::instructions::{pause, unpause};
use pinocchio::{AccountView, ProgramResult};

use crate::{
    constants::{AUTHORITY_SEED, GUARDIAN, ORACLE_AUTHORITY_DISCRIMINATOR},
    error::OracleError,
    state::RootAuthority,
};

/// Accounts: [root (signer), authority (writable)]. Any one root signer.
pub fn process_pause(accounts: &mut [AccountView]) -> ProgramResult {
    pause::<RootAuthority, OracleError>(
        accounts,
        &crate::ID,
        &[AUTHORITY_SEED, GUARDIAN.as_ref()],
        ORACLE_AUTHORITY_DISCRIMINATOR,
    )
}

/// Accounts: [guardian (signer), authority (writable), roots (signers) ..]. The guardian plus
/// enough distinct root signers, all in one transaction.
pub fn process_unpause(accounts: &mut [AccountView]) -> ProgramResult {
    unpause::<RootAuthority, OracleError>(
        accounts,
        &crate::ID,
        &[AUTHORITY_SEED, GUARDIAN.as_ref()],
        ORACLE_AUTHORITY_DISCRIMINATOR,
    )
}
