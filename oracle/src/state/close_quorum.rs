use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{
    Address, ProgramResult,
    sysvars::{Sysvar, clock::Clock},
};

use crate::{errors::OracleError, state::OracleAuthority};

pub const CLOSER_QUORUM_SEED: &[u8] = b"closer_quorum";

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy)]
pub struct CloserQuorum {
    pub initiator: [u8; 32], // public user can request to close the oracle all fees go to the initiator
    pub quorum: [[u8; 32]; 2], // two supermajority needs to vote on this
    pub quorum_completed_at: [u8; 8], // i64 time
}

impl CloserQuorum {
    pub const LEN: usize = core::mem::size_of::<CloserQuorum>();

    #[inline]
    pub fn new(initiator: Address) -> Self {
        Self {
            initiator: initiator.to_bytes(),
            quorum: [[0; 32]; 2],
            quorum_completed_at: 0u64.to_le_bytes(),
        }
    }

    #[inline]
    pub fn is_quorum_complete(&self) -> bool {
        u64::from_le_bytes(self.quorum_completed_at) != 0
    }

    #[inline]
    pub fn vote(&mut self, address: Address, authority: &OracleAuthority) -> ProgramResult {
        let clock = Clock::get()?;
        self.record_vote(address, authority, clock.unix_timestamp.to_le_bytes())
    }

    #[inline]
    fn record_vote(
        &mut self,
        address: Address,
        authority: &OracleAuthority,
        completed_at: [u8; 8],
    ) -> ProgramResult {
        // check that is_attest_complete is zero
        if self.is_quorum_complete() {
            return Err(OracleError::AttestationComplete.into());
        }

        let address = address.to_bytes();
        if address == [0; 32] {
            return Err(OracleError::InvalidAuthority.into());
        }

        // only supermajority can vote for the closure
        if !authority.supermajority.contains(&address) {
            return Err(OracleError::InvalidAuthority.into());
        }

        // check that address does not exist in the array.
        if self.quorum.contains(&address) {
            return Err(OracleError::DuplicateAuthority.into());
        }

        // push into the first empty attester slot.
        let attester_slot = self
            .quorum
            .iter_mut()
            .find(|stored_address| **stored_address == [0; 32])
            .ok_or(OracleError::AttestationComplete)?;
        *attester_slot = address;

        // if this push is the concluding one, set is_attest_complete.
        if !self.quorum.contains(&[0; 32]) {
            self.quorum_completed_at = completed_at;
        }

        Ok(())
    }
}
