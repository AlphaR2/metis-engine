// The signer/writable/owner/discriminator/back-reference checks that used
// to be duplicated by hand in every instruction. Centralized here so each
// check is independently testable and there's exactly one place to fix a
// mistake in how an account gets validated.

use pinocchio::{AccountView, Address, error::ProgramError};

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

/// The full pre-read pipeline for an account about to be interacted with.
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

// Tests for the shared account checks. To see the printed output:
//   cargo test -p openstake-core --lib validation -- --nocapture
//
// The signer/writable/owner checks can't be unit tested here - they need a
// real AccountView, which only exists inside a running program. Those are
// covered by the litesvm tests in tests/ instead. What's left are the two
// pure comparisons, which are worth testing directly because every
// instruction leans on them to prove it was handed the right accounts.
#[cfg(test)]
mod tests {
    use super::*;

    // The discriminator is what stops a Config being passed where a Market is
    // expected. Both accounts are just bytes owned by this program, so without
    // this check the wrong one would be read as the wrong type.
    #[test]
    fn discriminator_check_accepts_matching_and_rejects_mismatched() {
        let expected = [1u8, 2, 3, 4, 5, 6, 7, 8];

        println!(
            "matching discriminator {expected:?} -> {:?}",
            require_discriminator(expected, expected).is_ok()
        );
        assert!(require_discriminator(expected, expected).is_ok());

        let wrong = [0u8; 8];
        println!("wrong discriminator {wrong:?} vs {expected:?} -> rejected");
        assert!(require_discriminator(wrong, expected).is_err());
    }

    // Used everywhere an account has to be a specific address: the settle
    // authority, a market's own vault, the outcome mint for an index. A missing
    // check here is how a bettor ends up credited on someone else's market.
    #[test]
    fn address_match_check_accepts_equal_and_rejects_different() {
        let a = crate::constants::AUTHORITY;
        let b = crate::constants::AUTHORITY;
        println!("same address {a} -> accepted");
        assert!(require_addresses_match(&a, &b).is_ok());

        let zeroed = Address::default();
        println!("different address ({a} vs all-zero) -> rejected");
        assert!(require_addresses_match(&a, &zeroed).is_err());
    }
}
