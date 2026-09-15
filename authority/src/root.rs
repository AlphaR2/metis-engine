use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::{Address, error::ProgramError};

/// Fixed capacity, matches the current 3-person team exactly. Once Squads is live,
/// `signer_count` drops to 1 (the vault PDA) with no resize needed. Squads itself holds
/// however many real members you want behind that one address, so this array never needs to
/// grow past 3.
pub const MAX_ROOT_SIGNERS: usize = 3;

#[repr(C)]
#[derive(Debug, Clone, Copy, NoUninit, CheckedBitPattern)]
pub struct RootAuthority {
    pub signers: [[u8; 32]; MAX_ROOT_SIGNERS],
    pub signer_count: u8,
    /// Independent of `signers`. This address's only power anywhere in the system is
    /// co-signing Unpause. Rotating it requires the outgoing guardian's own signature (see each
    /// program's own SetGuardian instruction), otherwise, it is useless for other actions
    pub guardian: [u8; 32],
    pub paused: u8,
    pub discriminator: [u8; 8],
}

impl RootAuthority {
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// Builds a fresh RootAuthority from however many real signers a program starts with.
    /// `signer_count` is derived from `signers.len()`, not passed separately, so the two can
    /// never disagree. Unused slots up to MAX_ROOT_SIGNERS stay zeroed.
    
    pub fn new(
      signers: &[Address], 
      guardian: Address, 
      discriminator: [u8; 8]
    ) -> Self {
        debug_assert!(signers.len() <= MAX_ROOT_SIGNERS);
        let mut padded = [[0u8; 32]; MAX_ROOT_SIGNERS];
        for (slot, signer) in padded.iter_mut().zip(signers) {
            *slot = signer.to_bytes();
        }
        Self {
            signers: padded,
            signer_count: signers.len() as u8,
            guardian: guardian.to_bytes(),
            paused: 0, // 1 for paused, 0 for unpaused
            discriminator,
        }
    }

    #[inline]
    pub fn is_root(&self, address: Address) -> bool {
        self.signers[..self.signer_count as usize].contains(&address.to_bytes())
    }

    #[inline]
    pub fn is_guardian(&self, address: Address) -> bool {
        self.guardian == address.to_bytes()
    }

    #[inline]
    pub fn is_paused(&self) -> bool {
        self.paused != 0
    }

    /// Never more than 2 distinct root signatures required to unpause, never more than however
    /// many real root signers currently exist — collapses to 1 once signer_count == 1 (Squads),
    /// since 2 distinct signatures from a one-address root is impossible to satisfy. The
    /// Guardian co-signature (checked separately, in each program's Unpause instruction) is
    /// what keeps unpause genuinely two-party even in that case.
    ///
    #[inline]
    pub fn required_unpause_votes(&self) -> usize {
        core::cmp::min(2, self.signer_count as usize)
    }
}

/// Fails unless `root` is currently unpaused. This is a program pause mechanism to protect metis from running ix when we have compromised root keys. The pause mechanism is a "soft" pause, meaning that the program can still be called, but it will reject all instructions that require root authority. The only instruction that can be called while paused is Unpause, which requires a supermajority of root signers and the guardian to unpause the program.
/// Post mvp recommendation is this : `https://github.com/deanmlittle/sbpf-asm-abort.git`

#[inline]
pub fn require_not_paused<E: Into<ProgramError>>(
    root: &RootAuthority,
    err: E,
) -> Result<(), ProgramError> {
    if root.is_paused() {
        return Err(err.into());
    }
    Ok(())
}

/// The generalized form of oracle's CloserQuorum, used for Unpause — pausing itself stays
/// single-signer (see RootAuthority::is_root), only resuming needs this.
#[repr(C)]
#[derive(Debug, Clone, Copy, NoUninit, CheckedBitPattern)]
pub struct PauseQuorum {
    pub votes: [[u8; 32]; MAX_ROOT_SIGNERS],
}

impl PauseQuorum {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new() -> Self {
        Self {
            votes: [[0; 32]; MAX_ROOT_SIGNERS],
        }
    }

    #[inline]
    pub fn is_complete(&self, root: &RootAuthority) -> bool {
        self.votes.iter().filter(|v| **v != [0; 32]).count() >= root.required_unpause_votes()
    }

    /// Records one root member's vote to unpause.
    /// Rejects a voter who isn't root, rejects a duplicate vote, and errors rather than silently no-ops
    /// once complete.
    pub fn vote<E: Into<ProgramError>>(
        &mut self,
        voter: Address,
        root: &RootAuthority,
        not_root_err: E,
        duplicate_err: E,
        already_complete_err: E,
    ) -> Result<(), ProgramError> {

      //check for when quorum is already complete, if so return error
        if self.is_complete(root) {
            return Err(already_complete_err.into());
        }
        //check if voter is root, if not return error
        if !root.is_root(voter) {
            return Err(not_root_err.into());
        }

        let voter_bytes = voter.to_bytes();

        // return error if voter has already voted
        if self.votes.contains(&voter_bytes) {
            return Err(duplicate_err.into());
        }
        // is_complete() was false, so a zero slot is guaranteed to exist
        let slot = self
            .votes
            .iter_mut()
            .find(|v| **v == [0; 32])
            .expect("is_complete() was false, so an empty vote slot must exist");
        *slot = voter_bytes;
        Ok(())
    }
}

impl Default for PauseQuorum {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address {
        Address::from([byte; 32])
    }

    #[test]
    fn new_pads_unused_signer_slots_and_derives_count() {
        let root = RootAuthority::new(&[addr(1), addr(2)], addr(9), [7; 8]);
        assert_eq!(root.signer_count, 2);
        assert!(root.is_root(addr(1)));
        assert!(root.is_root(addr(2)));
        assert!(!root.is_root(addr(3)));
        assert_eq!(root.signers[2], [0; 32]);
    }

    #[test]
    fn required_unpause_votes_collapses_to_one_at_single_signer() {
        let three = RootAuthority::new(&[addr(1), addr(2), addr(3)], addr(9), [0; 8]);
        assert_eq!(three.required_unpause_votes(), 2);

        let one = RootAuthority::new(&[addr(1)], addr(9), [0; 8]);
        assert_eq!(one.required_unpause_votes(), 1);
    }

    #[test]
    fn pause_quorum_completes_after_required_distinct_votes() {
        let root = RootAuthority::new(&[addr(1), addr(2), addr(3)], addr(9), [0; 8]);
        let mut quorum = PauseQuorum::new();

        quorum
            .vote(addr(1), &root, ProgramError::Custom(1), ProgramError::Custom(2), ProgramError::Custom(3))
            .unwrap();
        assert!(!quorum.is_complete(&root));

        quorum
            .vote(addr(2), &root, ProgramError::Custom(1), ProgramError::Custom(2), ProgramError::Custom(3))
            .unwrap();
        assert!(quorum.is_complete(&root));
    }

    #[test]
    fn pause_quorum_rejects_non_root_and_duplicate_votes() {
        let root = RootAuthority::new(&[addr(1), addr(2), addr(3)], addr(9), [0; 8]);
        let mut quorum = PauseQuorum::new();

        assert!(
            quorum
                .vote(addr(99), &root, ProgramError::Custom(1), ProgramError::Custom(2), ProgramError::Custom(3))
                .is_err()
        );

        quorum
            .vote(addr(1), &root, ProgramError::Custom(1), ProgramError::Custom(2), ProgramError::Custom(3))
            .unwrap();
        assert!(
            quorum
                .vote(addr(1), &root, ProgramError::Custom(1), ProgramError::Custom(2), ProgramError::Custom(3))
                .is_err()
        );
    }

    #[test]
    fn single_signer_root_needs_only_its_own_vote_to_unpause() {
        let root = RootAuthority::new(&[addr(1)], addr(9), [0; 8]);
        let mut quorum = PauseQuorum::new();

        quorum
            .vote(addr(1), &root, ProgramError::Custom(1), ProgramError::Custom(2), ProgramError::Custom(3))
            .unwrap();
        assert!(quorum.is_complete(&root));
    }
}
