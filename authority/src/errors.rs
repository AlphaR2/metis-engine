//! Every named error kind the shared instruction functions in `instructions.rs` can produce.
//! Each consuming program implements this trait for its own concrete error enum (OracleError,
//! MetisError, ...) so the shared logic never has to know which one it's talking to

use pinocchio::error::ProgramError;

pub trait AuthorityErrors: Copy + Into<ProgramError> {

    /// A required signer didn't sign (attester, root, initializer).
    fn must_sign() -> Self;
    /// Not one of root's own signers, where root approval was required.
    fn unauthorized_authority() -> Self;
    /// The root-bearing account already exists / isn't empty at initialize time.
    fn already_initialized() -> Self;
    /// A derived PDA didn't match the account actually passed in.
    fn invalid_authority_pda() -> Self;
    /// An account's discriminator, stored address field, or general shape didn't check out.
    fn invalid_authority() -> Self;
    /// `Rent::try_minimum_balance` failed.
    fn rent_calculation_failed() -> Self;
    /// The same attester/voter tried to attest/vote twice.
    fn duplicate_authority() -> Self;
    /// Tried to attest/vote after the quorum was already complete.
    fn attestation_complete() -> Self;
    /// A non-root attester tried to finish off a candidate no root has touched yet.
    fn missing_root_attestation() -> Self;
    /// The program is paused: nothing that changes who is trusted may run.
    fn paused() -> Self;
    /// Unpause was called without the guardian plus the required number of distinct root signers.
    fn unpause_signatures_missing() -> Self;
}
