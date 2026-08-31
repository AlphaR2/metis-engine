// The signer/writable/owner/discriminator/back-reference checks that used
// to be duplicated by hand in every instruction. Centralized here so each
// check is independently testable and there's exactly one place to fix a
// mistake in how an account gets validated.

use pinocchio::{error::ProgramError, AccountView, Address};

use crate::error::OpenStakeError;

pub fn require_signer(account: &AccountView) -> Result<(), ProgramError> {
    if !account.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    Ok(())
}

pub fn require_writable(account: &AccountView) -> Result<(), ProgramError> {
    if !account.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

pub fn require_not_empty(account: &AccountView) -> Result<(), ProgramError> {
    if account.is_data_empty() {
        return Err(ProgramError::UninitializedAccount);
    }
    Ok(())
}

pub fn require_owned_by(account: &AccountView, program_id: &Address) -> Result<(), ProgramError> {
    if !account.owned_by(program_id) {
        return Err(ProgramError::IncorrectProgramId);
    }
    Ok(())
}

/// The full pre-read pipeline for an account this about to interact with

pub fn require_initialized_and_owned(
    account: &AccountView,
    program_id: &Address,
    must_be_writable: bool,
) -> Result<(), ProgramError> {
    if must_be_writable {
        require_writable(account)?;
    }
    require_not_empty(account)?;
    require_owned_by(account, program_id)?;
    Ok(())
}

pub fn require_discriminator(actual: [u8; 8], expected: [u8; 8]) -> Result<(), ProgramError> {
    if actual != expected {
        return Err(OpenStakeError::DiscriminatorMismatch.into());
    }
    Ok(())
}

pub fn require_addresses_match(actual: &Address, expected: &Address) -> Result<(), ProgramError> {
    if actual != expected {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminator_check_accepts_matching_and_rejects_mismatched() {
        let expected = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert!(require_discriminator(expected, expected).is_ok());
        assert!(require_discriminator([0u8; 8], expected).is_err());
    }

    #[test]
    fn address_match_check_accepts_equal_and_rejects_different() {
        let a = crate::constants::AUTHORITY;
        let b = crate::constants::AUTHORITY;
        assert!(require_addresses_match(&a, &b).is_ok());

        let zeroed = Address::default();
        assert!(require_addresses_match(&a, &zeroed).is_err());
    }
}
