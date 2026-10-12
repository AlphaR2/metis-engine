use metis_authority::instructions::{pause, unpause};
use pinocchio::{AccountView, ProgramResult};

use crate::{
    config::state::{CONFIG_DISCRIMINATOR, Config},
    constants::{CONFIG_SEED, GUARDIAN},
    error::MetisError,
};

/// Accounts: [root (signer), config (writable)]. Any one root signer.
///
/// Stops everything that creates a market, takes a bet, settles, sweeps or closes. It does not
/// stop claim, claim_refund or the two-root emergency cancel: pausing must never trap money.
pub fn process_pause(accounts: &mut [AccountView]) -> ProgramResult {
    pause::<Config, MetisError>(accounts, &crate::ID, &[CONFIG_SEED, GUARDIAN.as_ref()], CONFIG_DISCRIMINATOR)
}

/// Accounts: [guardian (signer), config (writable), roots (signers) ..]. The guardian plus
/// enough distinct root signers, all in one transaction.
pub fn process_unpause(accounts: &mut [AccountView]) -> ProgramResult {
    unpause::<Config, MetisError>(accounts, &crate::ID, &[CONFIG_SEED, GUARDIAN.as_ref()], CONFIG_DISCRIMINATOR)
}
