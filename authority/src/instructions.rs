//! Shared instruction bodies for the attest/reject delegation pattern. Generic over:
//!   - `E: AuthorityErrors` — each program's own concrete error enum (see errors.rs)
//!   - `T: RootBearing` — the account type that actually holds a RootAuthority. For oracle
//!     that's RootAuthority itself (a bare top-level account); for programs/core it's Config
//!     (RootAuthority embedded as `Config.root`). Neither program hand-writes this logic again.


use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{
    AccountView, Address, ProgramResult,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{Sysvar, rent::Rent},
};
use pinocchio_system::instructions::CreateAccount;

use crate::{delegated::DelegatedAuthority, errors::AuthorityErrors, root::RootAuthority};

/// Implemented by whatever account type actually carries a RootAuthority - either the
/// RootAuthority account itself (oracle) or a larger account embedding one as a field
/// (programs/core's Config). Lets attest_authority/remove_authority read and check root
/// without knowing which shape they're looking at.
pub trait RootBearing: CheckedBitPattern + NoUninit {
    fn root(&self) -> &RootAuthority;
    fn root_mut(&mut self) -> &mut RootAuthority;
    fn discriminator(&self) -> [u8; 8];
}


fn derive_root_pda(seeds: &[&[u8]], program_id: &Address) -> Option<(Address, u8)> {
    match seeds {
        [a] => Address::derive_program_address(&[*a], program_id),
        [a, b] => Address::derive_program_address(&[*a, *b], program_id),
        _ => None,
    }
}

impl RootBearing for RootAuthority {
    #[inline]
    fn root(&self) -> &RootAuthority {
        self
    }

    #[inline]
    fn root_mut(&mut self) -> &mut RootAuthority {
        self
    }

    #[inline]
    fn discriminator(&self) -> [u8; 8] {
        self.discriminator
    }
}

/// Accounts: [initializer, authority, system_program, ..]
/// Oracle-only - see module docs on why programs whose root is embedded in a larger account
/// (like Config) don't call this.
pub fn initialize_authority<E: AuthorityErrors>(
    accounts: &mut [AccountView],
    program_id: &Address,
    authority_seeds: &[&[u8]],
    root_signers: &[Address],
    guardian: Address,
    root_discriminator: [u8; 8],
) -> ProgramResult {
    let [initializer, authority, _system_program, _extra @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let authority_state = RootAuthority::new(root_signers, guardian, root_discriminator);

    if !initializer.is_signer() {
        return Err(E::must_sign().into());
    }
    if !authority_state.is_root(*initializer.address()) {
        return Err(E::unauthorized_authority().into());
    }

    if !authority.owned_by(&pinocchio_system::ID) || !authority.is_data_empty() {
        return Err(E::already_initialized().into());
    }

    let (derived, bump) = derive_root_pda(authority_seeds, program_id)
        .ok_or_else(|| E::invalid_authority_pda().into())?;
    if &derived != authority.address() {
        return Err(E::invalid_authority_pda().into());
    }

    let bump_arr = [bump];
    let rent_exempt = Rent::get()?
        .try_minimum_balance(RootAuthority::LEN)
        .map_err(|_| E::rent_calculation_failed().into())?;

    // A small fixed match instead of a dynamically-sized seed array - no_std/SBF context, and
    // in practice every consumer's root PDA is derived from either 1 or 2 seed components.
    match authority_seeds {
        [a] => {
            let seeds = [Seed::from(*a), Seed::from(&bump_arr)];
            CreateAccount {
                from: initializer,
                lamports: rent_exempt,
                owner: program_id,
                space: RootAuthority::LEN as u64,
                to: authority,
            }
            .invoke_signed(&[Signer::from(&seeds)])?;
        }
        [a, b] => {
            let seeds = [Seed::from(*a), Seed::from(*b), Seed::from(&bump_arr)];
            CreateAccount {
                from: initializer,
                lamports: rent_exempt,
                owner: program_id,
                space: RootAuthority::LEN as u64,
                to: authority,
            }
            .invoke_signed(&[Signer::from(&seeds)])?;
        }
        _ => return Err(E::invalid_authority_pda().into()),
    }

    let authority_data = unsafe { authority.borrow_unchecked_mut() };
    authority_data.copy_from_slice(bytemuck::bytes_of(&authority_state));
    Ok(())
}

/// Accounts: [attester, authority, candidate, individual_authority, system_program, remaining @ ..]
/// `remaining` holds at most one account: the attester's own DelegatedAuthority, required only
/// when the attester isn't root.
pub fn attest_authority<T: RootBearing, E: AuthorityErrors>(
    accounts: &mut [AccountView],
    program_id: &Address,
    authority_seeds: &[&[u8]],
    individual_authority_seed: &[u8],
    outer_discriminator: [u8; 8],
    delegated_discriminator: [u8; 8],
) -> ProgramResult {
    let [attester, authority, candidate, individual_authority, _system_program, remaining @ ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // Only ever one optional trailing account - reject anything beyond that instead of
    // silently ignoring it.
    if remaining.len() > 1 {
        return Err(ProgramError::InvalidInstructionData);
    }

    if !attester.is_signer() {
        return Err(E::must_sign().into());
    }

    let (expected_authority, _) = derive_root_pda(authority_seeds, program_id)
        .ok_or_else(|| E::invalid_authority_pda().into())?;
    if &expected_authority != authority.address() || !authority.owned_by(program_id) {
        return Err(E::invalid_authority_pda().into());
    }

    let authority_outer = bytemuck::checked::try_from_bytes::<T>(unsafe {
        authority.borrow_unchecked()
    })
    .map_err(|_| E::invalid_authority().into())?;
    if authority_outer.discriminator() != outer_discriminator {
        return Err(E::invalid_authority().into());
    }
    let authority_state = *authority_outer.root();
    // Who is trusted must not change while the program is paused: pausing exists for the case
    // where a root key is compromised, and attesting a new reporter is exactly what that
    // attacker would do next.
    if authority_state.is_paused() {
        return Err(E::paused().into());
    }

    let attester_is_root = authority_state.is_root(*attester.address());

    if !attester_is_root {
        let attester_individual_authority = remaining
            .first()
            .ok_or(ProgramError::NotEnoughAccountKeys)?;

        let (expected_attester_authority, _) = Address::derive_program_address(
            &[individual_authority_seed, attester.address().as_ref()],
            program_id,
        )
        .ok_or_else(|| E::unauthorized_authority().into())?;
        if expected_attester_authority != *attester_individual_authority.address() {
            return Err(E::unauthorized_authority().into());
        }
        if !attester_individual_authority.owned_by(program_id) {
            return Err(E::unauthorized_authority().into());
        }

        let attester_state = bytemuck::checked::try_from_bytes::<DelegatedAuthority>(unsafe {
            attester_individual_authority.borrow_unchecked()
        })
        .map_err(|_| E::unauthorized_authority().into())?;
        if attester_state.discriminator != delegated_discriminator {
            return Err(E::unauthorized_authority().into());
        }
        if attester_state.address != attester.address().to_bytes()
            || !attester_state.is_attest_complete()
        {
            return Err(E::unauthorized_authority().into());
        }
    }

    // Cannot add a root signer - they're already root.
    if authority_state.is_root(*candidate.address()) {
        return Err(E::invalid_authority().into());
    }

    let (expected_individual_authority, bump) = Address::derive_program_address(
        &[individual_authority_seed, candidate.address().as_ref()],
        program_id,
    )
    .ok_or_else(|| E::invalid_authority_pda().into())?;
    if expected_individual_authority != *individual_authority.address() {
        return Err(E::invalid_authority_pda().into());
    }

    if individual_authority.is_data_empty() {
        if !individual_authority.owned_by(&pinocchio_system::ID) {
            return Err(E::invalid_authority_pda().into());
        }

        let bump_arr = [bump];
        let candidate_address = candidate.address();
        let seeds = [
            Seed::from(individual_authority_seed),
            Seed::from(candidate_address.as_ref()),
            Seed::from(&bump_arr),
        ];
        let signer = Signer::from(&seeds);
        let rent_exempt = Rent::get()?
            .try_minimum_balance(DelegatedAuthority::LEN)
            .map_err(|_| E::rent_calculation_failed().into())?;

        CreateAccount {
            from: attester,
            lamports: rent_exempt,
            owner: program_id,
            space: DelegatedAuthority::LEN as u64,
            to: individual_authority,
        }
        .invoke_signed(&[signer])?;

        let individual_data = unsafe { individual_authority.borrow_unchecked_mut() };
        individual_data.copy_from_slice(bytemuck::bytes_of(&DelegatedAuthority::new(
            *candidate.address(),
            delegated_discriminator,
        )));
    } else if !individual_authority.owned_by(program_id) {
        return Err(E::invalid_authority_pda().into());
    }

    let individual_state = bytemuck::checked::try_from_bytes_mut::<DelegatedAuthority>(unsafe {
        individual_authority.borrow_unchecked_mut()
    })
    .map_err(|_| E::invalid_authority().into())?;
    if individual_state.discriminator != delegated_discriminator {
        return Err(E::invalid_authority().into());
    }
    if individual_state.address != candidate.address().to_bytes() {
        return Err(E::invalid_authority().into());
    }

    if !attester_is_root && !individual_state.has_root_attester(&authority_state) {
        return Err(E::missing_root_attestation().into());
    }

    individual_state.attest(
        *attester.address(),
        E::invalid_authority(),
        E::duplicate_authority(),
        E::attestation_complete(),
    )
}

/// Accounts: [root, authority, candidate, individual_authority, recipient, ..]
pub fn remove_authority<T: RootBearing, E: AuthorityErrors>(
    accounts: &mut [AccountView],
    program_id: &Address,
    authority_seeds: &[&[u8]],
    individual_authority_seed: &[u8],
    outer_discriminator: [u8; 8],
    delegated_discriminator: [u8; 8],
) -> ProgramResult {
    let [root, authority, candidate, individual_authority, recipient, _extra @ ..] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !root.is_signer() {
        return Err(E::must_sign().into());
    }

    let (expected_authority, _) = derive_root_pda(authority_seeds, program_id)
        .ok_or_else(|| E::invalid_authority_pda().into())?;
    if &expected_authority != authority.address() || !authority.owned_by(program_id) {
        return Err(E::invalid_authority_pda().into());
    }

    let authority_outer = bytemuck::checked::try_from_bytes::<T>(unsafe {
        authority.borrow_unchecked()
    })
    .map_err(|_| E::invalid_authority().into())?;
    if authority_outer.discriminator() != outer_discriminator {
        return Err(E::invalid_authority().into());
    }
    if authority_outer.root().is_paused() {
        return Err(E::paused().into());
    }
    if !authority_outer.root().is_root(*root.address()) {
        return Err(E::unauthorized_authority().into());
    }

    let (expected_individual_authority, _) = Address::derive_program_address(
        &[individual_authority_seed, candidate.address().as_ref()],
        program_id,
    )
    .ok_or_else(|| E::invalid_authority_pda().into())?;
    if expected_individual_authority != *individual_authority.address() {
        return Err(E::invalid_authority_pda().into());
    }
    if !individual_authority.owned_by(program_id) {
        return Err(E::invalid_authority_pda().into());
    }

    let individual_state = bytemuck::checked::try_from_bytes::<DelegatedAuthority>(unsafe {
        individual_authority.borrow_unchecked()
    })
    .map_err(|_| E::invalid_authority().into())?;
    if individual_state.discriminator != delegated_discriminator {
        return Err(E::invalid_authority().into());
    }
    if individual_state.address != candidate.address().to_bytes() {
        return Err(E::invalid_authority().into());
    }

    let recipient_lamports = recipient
        .lamports()
        .checked_add(individual_authority.lamports())
        .ok_or(ProgramError::InvalidArgument)?;
    recipient.set_lamports(recipient_lamports);
    individual_authority.set_lamports(0);
    individual_authority.close()
}

/// Loads the root-bearing account for a pause/unpause call, after the same PDA, ownership and
/// discriminator checks every other instruction here does.
fn load_root_bearing_mut<'a, T: RootBearing, E: AuthorityErrors>(
    authority: &'a mut AccountView,
    program_id: &Address,
    authority_seeds: &[&[u8]],
    outer_discriminator: [u8; 8],
) -> Result<&'a mut T, ProgramError> {
    let (expected_authority, _) = derive_root_pda(authority_seeds, program_id)
        .ok_or_else(|| E::invalid_authority_pda().into())?;
    if &expected_authority != authority.address() || !authority.owned_by(program_id) {
        return Err(E::invalid_authority_pda().into());
    }
    if !authority.is_writable() {
        return Err(E::invalid_authority_pda().into());
    }
    let outer = bytemuck::checked::try_from_bytes_mut::<T>(unsafe { authority.borrow_unchecked_mut() })
        .map_err(|_| E::invalid_authority().into())?;
    if outer.discriminator() != outer_discriminator {
        return Err(E::invalid_authority().into());
    }
    Ok(outer)
}

/// Accounts: [root, authority (writable)]
///
/// Any single root signer can pause: stopping things must be fast and cheap, since the case
/// it exists for is a key being compromised right now. Resuming is the hard part (`unpause`).
/// Pausing an already-paused program is a no-op, not an error, so a panicking operator can
/// resend without worrying.
pub fn pause<T: RootBearing, E: AuthorityErrors>(
    accounts: &mut [AccountView],
    program_id: &Address,
    authority_seeds: &[&[u8]],
    outer_discriminator: [u8; 8],
) -> ProgramResult {
    let [root, authority, _extra @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !root.is_signer() {
        return Err(E::must_sign().into());
    }
    let outer = load_root_bearing_mut::<T, E>(authority, program_id, authority_seeds, outer_discriminator)?;
    if !outer.root().is_root(*root.address()) {
        return Err(E::unauthorized_authority().into());
    }
    outer.root_mut().paused = 1;
    Ok(())
}

/// Accounts: [guardian, authority (writable), roots @ ..]
///
/// Needs the guardian AND `required_unpause_votes()` distinct root signers, all signing this one
/// transaction - no stored quorum to go stale or be griefed. The guardian alone can't resume
/// anything (its only power), and a compromised root set alone can't either.
pub fn unpause<T: RootBearing, E: AuthorityErrors>(
    accounts: &mut [AccountView],
    program_id: &Address,
    authority_seeds: &[&[u8]],
    outer_discriminator: [u8; 8],
) -> ProgramResult {
    let [guardian, authority, roots @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let outer = load_root_bearing_mut::<T, E>(authority, program_id, authority_seeds, outer_discriminator)?;
    let root_state = *outer.root();

    if !guardian.is_signer() || !root_state.is_guardian(*guardian.address()) {
        return Err(E::unpause_signatures_missing().into());
    }

    // Count distinct root signers; a key listed twice counts once.
    let mut seen = [[0u8; 32]; crate::root::MAX_ROOT_SIGNERS];
    let mut distinct = 0usize;
    for candidate in roots.iter() {
        if !candidate.is_signer() || !root_state.is_root(*candidate.address()) {
            continue;
        }
        let key = candidate.address().to_bytes();
        if seen[..distinct].contains(&key) {
            continue;
        }
        seen[distinct] = key;
        distinct += 1;
        if distinct == seen.len() {
            break;
        }
    }
    if distinct < root_state.required_unpause_votes() {
        return Err(E::unpause_signatures_missing().into());
    }

    outer.root_mut().paused = 0;
    Ok(())
}
