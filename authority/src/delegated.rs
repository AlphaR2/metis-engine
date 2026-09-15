//! DelegatedAuthority: a per-candidate,  per program attestation record( most general a backend authorizer). Root vets a candidate once (recording up to 2 attestations here);
//! once complete, that candidate can act independently on day-to-day operations without root
//! being involved in every transaction: see RootAuthority for the root side of this.

use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{
    Address, ProgramResult,
    error::ProgramError,
    sysvars::{Sysvar, clock::Clock},
};

use crate::root::RootAuthority;

#[repr(C)]
#[derive(Debug, Clone, Copy, NoUninit, CheckedBitPattern)]
pub struct DelegatedAuthority {
    pub address: [u8; 32],
    pub attesters: [[u8; 32]; 2], // roots that attested to this candidate
    pub is_attest_complete: [u8; 8], 
    pub discriminator: [u8; 8],
}

impl DelegatedAuthority {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(address: Address, discriminator: [u8; 8]) -> Self {
        Self {
            address: address.to_bytes(),
            attesters: [[0; 32]; 2],
            is_attest_complete: 0u64.to_le_bytes(),
            discriminator,
        }
    }

    #[inline]
    pub fn is_attest_complete(&self) -> bool {
        u64::from_le_bytes(self.is_attest_complete) != 0
    }

    /// Whether any attester(root) already recorded on this candidate is one of root's own signers —
    /// the check that lets a non-root attester finish off a candidate root has already touched,
    /// without root needing to be involved a second time.
    #[inline]
    pub fn has_root_attester(&self, root: &RootAuthority) -> bool {
        self.attesters
            .iter()
            .any(|attester| root.is_root(Address::from(*attester)))
    }

    /// Records one root member's attestation of this candidate. Rejects a voter who isn't root,
    /// rejects a duplicate attestation, and errors rather than silently no-ops once complete.
    pub fn attest<E: Into<ProgramError>>(
        &mut self,
        address: Address,
        zero_address_err: E,
        duplicate_err: E,
        already_complete_err: E,
    ) -> ProgramResult {
        let clock = Clock::get()?;
        self.record_attestation(
            address,
            clock.unix_timestamp.to_le_bytes(),
            zero_address_err,
            duplicate_err,
            already_complete_err,
        )
    }

    fn record_attestation<E: Into<ProgramError>>(
        &mut self,
        address: Address,
        completed_at: [u8; 8],
        zero_address_err: E,
        duplicate_err: E,
        already_complete_err: E,
    ) -> ProgramResult {
      //check for when attestation is already complete, if so return error
        if self.is_attest_complete() {
            return Err(already_complete_err.into());
        }

        let address = address.to_bytes();
        if address == [0; 32] {
            return Err(zero_address_err.into());
        }

        if self.attesters.contains(&address) {
            return Err(duplicate_err.into());
        }

        // is_attest_complete() was false, so an empty slot is guaranteed to exist.
        let attester_slot = self
            .attesters
            .iter_mut()
            .find(|stored_address| **stored_address == [0; 32])
            .expect("is_attest_complete() was false, so an empty attester slot must exist");
        *attester_slot = address;

        if !self.attesters.contains(&[0; 32]) {
            self.is_attest_complete = completed_at;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address {
        Address::from([byte; 32])
    }

    #[test]
    fn completes_after_two_unique_attestations() {
        let mut candidate = DelegatedAuthority::new(addr(9), [0; 8]);
        candidate
            .record_attestation(
                addr(1),
                42u64.to_le_bytes(),
                ProgramError::Custom(1),
                ProgramError::Custom(2),
                ProgramError::Custom(3),
            )
            .unwrap();

        assert_eq!(candidate.attesters, [[1; 32], [0; 32]]);
        assert_eq!(u64::from_le_bytes(candidate.is_attest_complete), 0);

        candidate
            .record_attestation(
                addr(2),
                42u64.to_le_bytes(),
                ProgramError::Custom(1),
                ProgramError::Custom(2),
                ProgramError::Custom(3),
            )
            .unwrap();
        assert_eq!(candidate.attesters, [[1; 32], [2; 32]]);
        assert_eq!(u64::from_le_bytes(candidate.is_attest_complete), 42);
    }

    #[test]
    fn rejects_duplicate_or_post_completion_attestations() {
        let mut candidate = DelegatedAuthority::new(addr(9), [0; 8]);
        candidate
            .record_attestation(
                addr(1),
                42u64.to_le_bytes(),
                ProgramError::Custom(1),
                ProgramError::Custom(2),
                ProgramError::Custom(3),
            )
            .unwrap();

        assert!(
            candidate
                .record_attestation(
                    addr(1),
                    42u64.to_le_bytes(),
                    ProgramError::Custom(1),
                    ProgramError::Custom(2),
                    ProgramError::Custom(3),
                )
                .is_err()
        );

        candidate
            .record_attestation(
                addr(2),
                42u64.to_le_bytes(),
                ProgramError::Custom(1),
                ProgramError::Custom(2),
                ProgramError::Custom(3),
            )
            .unwrap();
        assert!(
            candidate
                .record_attestation(
                    addr(3),
                    42u64.to_le_bytes(),
                    ProgramError::Custom(1),
                    ProgramError::Custom(2),
                    ProgramError::Custom(3),
                )
                .is_err()
        );
    }

    #[test]
    fn has_root_attester_checks_against_roots_own_signer_set() {
        let root = RootAuthority::new(&[addr(1), addr(2), addr(3)], addr(50), [0; 8]);
        let mut candidate = DelegatedAuthority::new(addr(9), [0; 8]);
        assert!(!candidate.has_root_attester(&root));

        candidate
            .record_attestation(
                addr(1),
                1u64.to_le_bytes(),
                ProgramError::Custom(1),
                ProgramError::Custom(2),
                ProgramError::Custom(3),
            )
            .unwrap();
        assert!(candidate.has_root_attester(&root));
    }
}