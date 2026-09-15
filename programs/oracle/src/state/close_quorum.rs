use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{
    Address, ProgramResult,
    sysvars::{Sysvar, clock::Clock},
};

use crate::error::OracleError;
use metis_authority::root::{PauseQuorum, RootAuthority};

#[repr(C)]
#[derive(NoUninit, CheckedBitPattern, Clone, Copy)]
pub struct CloserQuorum {
    pub initiator: [u8; 32], // public user can request to close the oracle - all fees go to the initiator
    /// Reuses metis-authority's PauseQuorum rather than a hand-rolled 2-slot array: the vote
    /// count needed to close scales with root's actual signer_count (min(2, signer_count)), so
    /// this stays satisfiable once root collapses from 3 raw keys down to a single Squads vault
    /// - a fixed "always needs 2" check would make close_oracle permanently unusable at that point.
    pub votes: PauseQuorum,
    pub quorum_completed_at: [u8; 8], // i64 time
}

impl CloserQuorum {
    pub const LEN: usize = core::mem::size_of::<CloserQuorum>();

    #[inline]
    pub fn new(initiator: Address) -> Self {
        Self {
            initiator: initiator.to_bytes(),
            votes: PauseQuorum::new(),
            quorum_completed_at: 0u64.to_le_bytes(),
        }
    }

    #[inline]
    pub fn is_quorum_complete(&self) -> bool {
        u64::from_le_bytes(self.quorum_completed_at) != 0
    }

    #[inline]
    pub fn vote(&mut self, address: Address, authority: &RootAuthority) -> ProgramResult {
        let clock = Clock::get()?;
        self.record_vote(address, authority, clock.unix_timestamp.to_le_bytes())
    }

    #[inline]
    fn record_vote(
        &mut self,
        address: Address,
        authority: &RootAuthority,
        completed_at: [u8; 8],
    ) -> ProgramResult {
        if self.is_quorum_complete() {
            return Err(OracleError::AttestationComplete.into());
        }
        //reuse metis-authority's PauseQuorum vote() method to record the vote and check for errors
        self.votes.vote(
            address,
            authority,
            OracleError::InvalidAuthority,
            OracleError::DuplicateAuthority,
            OracleError::AttestationComplete,
        )?;

        if self.votes.is_complete(authority) {
            self.quorum_completed_at = completed_at;
        }

        Ok(())
    }
}
