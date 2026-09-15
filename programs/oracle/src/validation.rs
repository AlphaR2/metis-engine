// The signer/writable/owner/PDA/unpack checks that used to be duplicated by hand across
// instructions/*. Mirrors programs/core/src/validation/account.rs's shape, plus a couple of
// oracle-specific composites that actually collapse the duplication found in this crate.

use bytemuck::CheckedBitPattern;
use pinocchio::{AccountView, Address, error::ProgramError};

use crate::{constants::INDIVIDUAL_AUTHORITY_SEED, error::OracleError};

pub fn require_signer(account: &AccountView, err: OracleError) -> Result<(), ProgramError> {
    if !account.is_signer() {
        return Err(err.into());
    }
    Ok(())
}

#[allow(dead_code)]
pub fn require_writable(account: &AccountView, err: OracleError) -> Result<(), ProgramError> {
    if !account.is_writable() {
        return Err(err.into());
    }
    Ok(())
}

pub fn require_empty(account: &AccountView, err: OracleError) -> Result<(), ProgramError> {
    if !account.is_data_empty() {
        return Err(err.into());
    }
    Ok(())
}

pub fn require_not_empty(account: &AccountView, err: OracleError) -> Result<(), ProgramError> {
    if account.is_data_empty() {
        return Err(err.into());
    }
    Ok(())
}

pub fn require_owned_by(
    account: &AccountView,
    program_id: &Address,
    err: OracleError,
) -> Result<(), ProgramError> {
    if !account.owned_by(program_id) {
        return Err(err.into());
    }
    Ok(())
}


pub fn require_discriminator(actual: [u8; 8], expected: [u8; 8]) -> Result<(), ProgramError> {
    if actual != expected {
        return Err(OracleError::InvalidGenericOracleData.into());
    }
    Ok(())
}

pub fn require_addresses_match(
    actual: &Address,
    expected: &Address,
    err: OracleError,
) -> Result<(), ProgramError> {
    if actual != expected {
        return Err(err.into());
    }
    Ok(())
}

/// Derives a PDA from `seeds` and checks it matches `expected`, returning the bump on success.
pub fn derive_and_check_pda<const N: usize>(
    seeds: &[&[u8]; N],
    program_id: &Address,
    expected: &Address,
    err: OracleError,
) -> Result<u8, ProgramError> {
    let (derived, bump) = Address::derive_program_address(seeds, program_id).ok_or(err)?;
    require_addresses_match(&derived, expected, err)?;
    Ok(bump)
}

/// The individual-authority PDA for a given candidate address.
pub fn individual_authority_address(
    candidate: &Address,
    program_id: &Address,
) -> Result<(Address, u8), ProgramError> {
    Address::derive_program_address(&[INDIVIDUAL_AUTHORITY_SEED, candidate.as_ref()], program_id)
        .ok_or(OracleError::InvalidAuthorityPda.into())
}

/// `bytemuck::checked::try_from_bytes::<T>` with the crate's own error type instead of
/// bytemuck's, matching every other error path in this crate. 
pub fn unpack<T: CheckedBitPattern>(data: &[u8], err: OracleError) -> Result<&T, ProgramError> {
    bytemuck::checked::try_from_bytes::<T>(data).map_err(|_| err.into())
}

pub fn unpack_mut<T: CheckedBitPattern + bytemuck::NoUninit>(
    data: &mut [u8],
    err: OracleError,
) -> Result<&mut T, ProgramError> {
    bytemuck::checked::try_from_bytes_mut::<T>(data).map_err(|_| err.into())
}

/// The `data.get(..N).ok_or(...).try_into().map_err(...)` prefix-extraction pattern duplicated
/// across create_oracle.rs, update_oracle.rs, football/create.rs, football/update.rs.
pub fn parse_prefix<const N: usize>(data: &[u8]) -> Result<&[u8; N], ProgramError> {
    data.get(..N)
        .ok_or(ProgramError::InvalidInstructionData)?
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_match_check_accepts_equal_and_rejects_different() {
        let a = Address::from([7u8; 32]);
        let b = Address::from([7u8; 32]);
        assert!(require_addresses_match(&a, &b, OracleError::InvalidAuthority).is_ok());

        let zeroed = Address::default();
        assert!(require_addresses_match(&a, &zeroed, OracleError::InvalidAuthority).is_err());
    }

    #[test]
    fn discriminator_check_accepts_matching_and_rejects_mismatched() {
        let expected = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert!(require_discriminator(expected, expected).is_ok());

        let wrong = [0u8; 8];
        assert!(require_discriminator(wrong, expected).is_err());
    }
}
