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

/// The one Config account: rebuilt from its seeds rather than trusted, then checked to exist
/// and belong to this program. The caller still checks the discriminator after borrowing.
pub fn require_config_account(config: &AccountView, must_be_writable: bool) -> Result<(), ProgramError> {
    let (config_pda, _) = Address::derive_program_address(
        &[crate::constants::CONFIG_SEED, crate::constants::GUARDIAN.as_ref()],
        &crate::ID,
    )
    .ok_or(MetisError::InvalidPda)?;
    require_addresses_match(config.address(), &config_pda, MetisError::InvalidPda)?;
    require_initialized_and_owned(config, &crate::ID, must_be_writable, MetisError::InvalidConfigAccount)
}

/// For the high-friction actions (critical config fields, overrides, cancels): both accounts
/// must sign, both must be current root signers, and they must be two different keys - one
/// compromised key passed twice doesn't count.
pub fn require_two_root_signers(
    root: &metis_authority::root::RootAuthority,
    first: &AccountView,
    second: &AccountView,
) -> Result<(), ProgramError> {
    if !first.is_signer()
        || !second.is_signer()
        || first.address() == second.address()
        || !root.is_root(*first.address())
        || !root.is_root(*second.address())
    {
        return Err(MetisError::TwoRootSignersRequired.into());
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
        let a = crate::constants::GUARDIAN;
        let b = crate::constants::GUARDIAN;
        println!("same address {a} -> accepted");
        assert!(require_addresses_match(&a, &b, MetisError::WrongAccountReference).is_ok());

        let zeroed = Address::default();
        println!("different address ({a} vs all-zero) -> rejected");
        assert!(require_addresses_match(&a, &zeroed, MetisError::WrongAccountReference).is_err());
    }
}

/// Reads Config's pause flag. Everything that moves money in, creates a market, settles or
/// sweeps checks it; user exits (claim, claim_refund) and the two-root emergency cancel do not -
/// pausing must never trap anyone's money.
pub fn require_not_paused(config: &AccountView) -> Result<(), ProgramError> {
    let data = config.try_borrow()?;
    let state = unpack::<crate::config::state::Config>(&data, MetisError::InvalidConfigAccount)?;
    require_discriminator(state.discriminator, crate::config::state::CONFIG_DISCRIMINATOR)?;
    if state.root.is_paused() {
        return Err(MetisError::ProgramPaused.into());
    }
    Ok(())
}

/// A copy of the oracle's football account, after checking it is the real thing: owned by the
/// oracle program, exactly the right size, right discriminator. The address is NOT checked
/// here - callers compare it to the one the market recorded.
pub fn read_oracle(oracle: &AccountView) -> Result<oracle::instructions::football::FootballSettled, ProgramError> {
    if !oracle.owned_by(&oracle::ID) {
        return Err(MetisError::InvalidOracleAccount.into());
    }
    let data = oracle.try_borrow()?;
    let state = checked::try_from_bytes::<oracle::instructions::football::FootballSettled>(&data)
        .map_err(|_| MetisError::InvalidOracleAccount)?;
    if state.discriminator != oracle::constants::FOOTBALL_SETTLED_DISCRIMINATOR {
        return Err(MetisError::InvalidOracleAccount.into());
    }
    Ok(*state)
}

/// Whether `signer` may run the gated crank instructions: a root, or a delegate that two roots
/// attested (the same attest/remove flow every delegate in the system uses, so an operator key can
/// be revoked the way any delegate is). `delegated` is the signer's own attestation account, only
/// needed when the signer isn't a root.
///
/// Reads only: the signer's actual signature is checked separately by the caller, because some
/// instructions open to anyone after a delay and must not demand a signer then.
pub fn is_operator(
    signer: &AccountView,
    delegated: Option<&AccountView>,
    config: &AccountView,
) -> Result<bool, ProgramError> {
    let config_data = config.try_borrow()?;
    let config_state = unpack::<crate::config::state::Config>(&config_data, MetisError::InvalidConfigAccount)?;
    require_discriminator(config_state.discriminator, crate::config::state::CONFIG_DISCRIMINATOR)?;
    if config_state.root.is_root(*signer.address()) {
        return Ok(true);
    }
    let Some(delegated) = delegated else { return Ok(false) };
    // Only this program can create an account it owns, so owner + discriminator + the address it
    // records is proof it is a real attestation of this very signer.
    if !delegated.owned_by(&crate::ID) {
        return Ok(false);
    }
    let data = delegated.try_borrow()?;
    let Ok(state) = checked::try_from_bytes::<metis_authority::delegated::DelegatedAuthority>(&data) else {
        return Ok(false);
    };
    Ok(state.discriminator == crate::config::state::DELEGATED_AUTHORITY_DISCRIMINATOR
        && state.address == signer.address().to_bytes()
        && state.is_attest_complete())
}

/// The gate: the signer must have signed, and must be a root or an attested operator.
pub fn require_operator(
    signer: &AccountView,
    delegated: Option<&AccountView>,
    config: &AccountView,
) -> Result<(), ProgramError> {
    require_signer(signer, MetisError::AuthorityMustSign)?;
    if is_operator(signer, delegated, config)? {
        Ok(())
    } else {
        Err(MetisError::OperatorRequired.into())
    }
}
