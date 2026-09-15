// The signer/writable/owner/discriminator/back-reference checks that used
// to be duplicated by hand in every instruction. Centralized here so each
// check is independently testable and there's exactly one place to fix a
// mistake in how an account gets validated.

use pinocchio::{AccountView, Address, error::ProgramError};
use bytemuck::{CheckedBitPattern, NoUninit, checked};

use crate::error::MetisError;

// Each primitive takes an explicit MetisError rather than returning a generic ProgramError -
// mirrors oracle/'s validation.rs, which already gave nearly every check its own domain-specific
// code (CreatorMustSign, OracleAlreadyInitialized, ...). A client parsing the returned error code
// gets a meaningful reason, not one shared "invalid account data" for every kind of failure.

pub fn require_signer(account: &AccountView, err: MetisError) -> Result<(), ProgramError> {
    if !account.is_signer() {
        return Err(err.into());
    }
    Ok(())
}

pub fn require_writable(account: &AccountView, err: MetisError) -> Result<(), ProgramError> {
    if !account.is_writable() {
        return Err(err.into());
    }
    Ok(())
}

pub fn require_not_empty(account: &AccountView, err: MetisError) -> Result<(), ProgramError> {
    if account.is_data_empty() {
        return Err(err.into());
    }
    Ok(())
}

pub fn require_owned_by(
    account: &AccountView,
    program_id: &Address,
    err: MetisError,
) -> Result<(), ProgramError> {
    if !account.owned_by(program_id) {
        return Err(err.into());
    }
    Ok(())
}

/// The full pre-read pipeline for an account about to be interacted with.
pub fn require_initialized_and_owned(
    account: &AccountView,
    program_id: &Address,
    must_be_writable: bool,
    err: MetisError,
) -> Result<(), ProgramError> {
    if must_be_writable {
        require_writable(account, err)?;
    }
    require_not_empty(account, err)?;
    require_owned_by(account, program_id, err)?;
    Ok(())
}

/// The one check that keeps a single, always-reused meaning ("the account read back wasn't the
/// type we expected") regardless of call site - no per-call-site variant needed.
pub fn require_discriminator(actual: [u8; 8], expected: [u8; 8]) -> Result<(), ProgramError> {
    if actual != expected {
        return Err(MetisError::DiscriminatorMismatch.into());
    }
    Ok(())
}

pub fn require_addresses_match(
    actual: &Address,
    expected: &Address,
    err: MetisError,
) -> Result<(), ProgramError> {
    if actual != expected {
        return Err(err.into());
    }
    Ok(())
}

pub fn unpack<T: CheckedBitPattern> (
    data: &[u8],
    err:MetisError
) -> Result<&T, ProgramError> {
    checked::try_from_bytes::<T>(data).map_err(|_| err.into())

}


pub fn unpack_mut<T: CheckedBitPattern + NoUninit>(
    data: &mut [u8],
    err: MetisError,
) -> Result<&mut T, ProgramError> {
    checked::try_from_bytes_mut::<T>(data).map_err(|_| err.into())
}



// Tests for the shared account checks. To see the printed output:
//   cargo test -p metis-core --lib validation -- --nocapture
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
        assert!(require_addresses_match(&a, &b, MetisError::WrongAccountReference).is_ok());

        let zeroed = Address::default();
        println!("different address ({a} vs all-zero) -> rejected");
        assert!(require_addresses_match(&a, &zeroed, MetisError::WrongAccountReference).is_err());
    }
}
