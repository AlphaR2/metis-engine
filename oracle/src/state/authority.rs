use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{
    Address, ProgramResult,
    sysvars::{Sysvar, clock::Clock},
};

use crate::errors::OracleError;

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy)]
pub struct OracleAuthority {
    pub supermajority: [[u8; 32]; 3],
}

pub const SUPERMAJORITIES: [[u8; 32]; 3] = [
    Address::from_str_const("UPDTR3Mmf4qQatb5qy8rE7aEw34Jq2AUJknEgbTuw8B").to_bytes(),
    Address::from_str_const("UPdtRERKVQhyEEFmxTUmmezz1BStBS1wiXn1h74bhRG").to_bytes(),
    Address::from_str_const("upDTrFkAvEEk2N9JoxbeY6jg1KHJNzu8BAqdf7f2xVc").to_bytes(),
];

impl OracleAuthority {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn hardcoded() -> Self {
        Self {
            supermajority: SUPERMAJORITIES,
        }
    }

    pub fn new(supermajority: [Address; 3]) -> Self {
        let [first, second, third] = supermajority;
        Self {
            supermajority: [first.to_bytes(), second.to_bytes(), third.to_bytes()],
        }
    }

    pub fn is_supermajority(&self, address: Address) -> bool {
        self.supermajority.contains(&address.to_bytes())
    }
}

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy)]
pub struct IndividualOAuthority {
    pub address: [u8; 32], // address
    pub attesters: [[u8; 32]; 2],
    pub is_attest_complete: [u8; 8], // i64 time
}

impl IndividualOAuthority {
    pub fn new(address: Address) -> Self {
        Self {
            address: address.to_bytes(),
            attesters: [[0; 32]; 2],
            is_attest_complete: 0u64.to_le_bytes(),
        }
    }

    pub const LEN: usize = core::mem::size_of::<IndividualOAuthority>();

    pub fn is_attest_complete(&self) -> bool {
        u64::from_le_bytes(self.is_attest_complete) != 0
    }

    pub fn has_supermajority_attester(&self, authority: &OracleAuthority) -> bool {
        self.attesters
            .iter()
            .any(|attester| authority.supermajority.contains(attester))
    }

    pub fn attest(&mut self, address: Address) -> ProgramResult {
        let clock = Clock::get()?;
        self.record_attestation(address, clock.unix_timestamp.to_le_bytes())
    }

    fn record_attestation(&mut self, address: Address, completed_at: [u8; 8]) -> ProgramResult {
        // check that is_attest_complete is zero
        if self.is_attest_complete() {
            return Err(OracleError::AttestationComplete.into());
        }

        let address = address.to_bytes();
        if address == [0; 32] {
            return Err(OracleError::InvalidAuthority.into());
        }

        // check that address does not exist in the array.
        if self.attesters.contains(&address) {
            return Err(OracleError::DuplicateAuthority.into());
        }

        // push into the first empty attester slot.
        let attester_slot = self
            .attesters
            .iter_mut()
            .find(|stored_address| **stored_address == [0; 32])
            .ok_or(OracleError::AttestationComplete)?;
        *attester_slot = address;

        // if this push is the concluding one, set is_attest_complete.
        if !self.attesters.contains(&[0; 32]) {
            self.is_attest_complete = completed_at;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{IndividualOAuthority, OracleAuthority};
    use crate::errors::OracleError;
    use pinocchio::{Address, error::ProgramError};

    fn authority(byte: u8) -> Address {
        Address::from([byte; 32])
    }

    #[test]
    fn records_exactly_two_roots() {
        let authorities = OracleAuthority::new([authority(1), authority(2), authority(3)]);

        assert!(authorities.is_supermajority(authority(1)));
        assert!(authorities.is_supermajority(authority(2)));
        assert!(!authorities.is_supermajority(authority(4)));
    }

    #[test]
    fn completes_after_two_unique_attestations() {
        let mut candidate = IndividualOAuthority::new(authority(9));
        candidate
            .record_attestation(authority(1), 42u64.to_le_bytes())
            .unwrap();

        assert_eq!(candidate.attesters, [[1; 32], [0; 32]]);
        assert_eq!(u64::from_le_bytes(candidate.is_attest_complete), 0);

        candidate
            .record_attestation(authority(2), 42u64.to_le_bytes())
            .unwrap();
        assert_eq!(candidate.attesters, [[1; 32], [2; 32]]);
        assert_eq!(u64::from_le_bytes(candidate.is_attest_complete), 42);
    }

    #[test]
    fn rejects_duplicate_or_post_completion_attestations() {
        let mut candidate = IndividualOAuthority::new(authority(9));
        candidate
            .record_attestation(authority(1), 42u64.to_le_bytes())
            .unwrap();

        assert_eq!(
            candidate.record_attestation(authority(1), 42u64.to_le_bytes()),
            Err(ProgramError::from(OracleError::DuplicateAuthority))
        );

        candidate
            .record_attestation(authority(2), 42u64.to_le_bytes())
            .unwrap();
        assert_eq!(
            candidate.record_attestation(authority(3), 42u64.to_le_bytes()),
            Err(ProgramError::from(OracleError::AttestationComplete))
        );
    }
}
