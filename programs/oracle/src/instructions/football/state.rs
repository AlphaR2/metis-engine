use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::ProgramResult;
#[cfg(target_os = "solana")]
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::{
    constants::{FOOTBALL_SETTLED_DISCRIMINATOR, SCORE_NIBBLE_MASK, SCORE_NIBBLE_MAX, CLOSER_QUORUM_SEED},
    error::OracleError,
    state::MarketKind,
};
use shank::{ShankAccount, ShankType};

#[repr(C)]
#[derive(Default, Clone, Copy, NoUninit, CheckedBitPattern, ShankAccount)]
pub struct FootballSettled {
    pub market_kind: MarketKind,
    /// Immutable commitment to the provider fixture aliases for this event.
    /// Every Oracle account stores this at bytes 1..33, immediately after its
    /// one-byte market kind, so generic instructions need not decode
    /// sport-specific fields.
    pub fixture_merkle_root: [u8; 32],
    pub score: u8, // this is a bitpack - higher 4 bits is home, lower 4 bits is away
    pub penalties: u8, // this is a bitpack - higher 4 bits is home, lower 4 bits is away
    pub status: CFootballMatchPhase, // 1 byte
    pub updated_at_unix: [u8; 8], // u64
    pub discriminator: [u8; 8],
}

// Every non-Scheduled variant is only ever constructed by tests today (real phase transitions
// are driven by an off-chain oracle client that isn't built yet). 
#[allow(dead_code)]
#[repr(u8)]
#[derive(Clone, Copy, Default, NoUninit, CheckedBitPattern, PartialEq, ShankType)]
pub enum CFootballMatchPhase {
    #[default]
    Scheduled = 0,
    FirstHalf = 1,
    HalfTime = 2,
    SecondHalf = 3,
    ExtraTimeFirstHalf = 4,
    ExtraTimeBreak = 5,
    ExtraTimeSecondHalf = 6,
    Penalties = 7,
    Finished = 8,
}

impl CFootballMatchPhase {
    /// Whether an oracle may move from this phase to `next`.
    ///
    /// Re-sending the current phase is allowed so an idempotent oracle retry
    /// does not fail after its first transaction has already landed.
    #[inline]
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }

        matches!(
            (self, next),
            (Self::Scheduled, Self::FirstHalf)
                | (Self::FirstHalf, Self::HalfTime)
                | (Self::HalfTime, Self::SecondHalf)
                | (Self::SecondHalf, Self::Finished)
                | (Self::SecondHalf, Self::ExtraTimeFirstHalf)
                | (Self::ExtraTimeFirstHalf, Self::ExtraTimeBreak)
                | (Self::ExtraTimeBreak, Self::ExtraTimeSecondHalf)
                | (Self::ExtraTimeSecondHalf, Self::Finished)
                | (Self::ExtraTimeSecondHalf, Self::Penalties)
                | (Self::Penalties, Self::Finished)
        )
    }
}

// Same story as CFootballMatchPhase above
#[allow(dead_code)]
impl FootballSettled {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(fixture_merkle_root: [u8; 32]) -> Self {
        let current_time = {
            #[cfg(target_os = "solana")]
            {
                let clock = Clock::get().expect("could  not get time");
                let current_time = clock.unix_timestamp as u64;
                current_time
            }

            #[cfg(not(target_os = "solana"))]
            {
                std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .expect("time went backwards")
                    .as_secs()
            }
        };

        Self {
            market_kind: MarketKind::Football,
            fixture_merkle_root,
            score: 0x00,
            penalties: 0x00,
            status: CFootballMatchPhase::Scheduled,
            updated_at_unix: current_time.to_le_bytes(),
            discriminator: FOOTBALL_SETTLED_DISCRIMINATOR,
        }
    }

    // clippy::wrong_self_convention: a `to_*` method on a Copy type is expected to consume
    // self by value; this one just borrows, so it's named as_bytes instead.
    #[inline]
    pub fn as_bytes(&self) -> &[u8; Self::LEN] {
        bytemuck::bytes_of(self).try_into().unwrap()
    }

    #[inline]
    pub fn home_score(&self) -> u8 {
        self.score >> 4
    }

    #[inline]
    pub fn away_score(&self) -> u8 {
        self.score & 0x0F
    }

    #[inline]
    pub fn get_score(&self) -> (u8, u8) {
        let home_score = self.home_score();
        let away_score = self.away_score();
        (home_score, away_score)
    }

    #[inline]
    pub fn set_scores(&mut self, home: u8, away: u8) {
        let home_score = home.min(SCORE_NIBBLE_MAX);
        let away_score = away.min(SCORE_NIBBLE_MAX);
        self.score = (home_score << 4) | (away_score & SCORE_NIBBLE_MASK);
    }

    #[inline]
    pub fn set_penalties(&mut self, home: u8, away: u8) {
        let home_score = home.min(SCORE_NIBBLE_MAX);
        let away_score = away.min(SCORE_NIBBLE_MAX);
        self.penalties = (home_score << 4) | (away_score & SCORE_NIBBLE_MASK);
    }

    #[inline]
    pub fn set_match_status(&mut self, status: CFootballMatchPhase) -> ProgramResult {
        if !self.status.can_transition_to(status) {
            return Err(OracleError::InvalidStateTransition.into());
        }

        self.status = status;
        Ok(())
    }

}

#[cfg(test)]
mod tests {
    use super::{CFootballMatchPhase, FootballSettled};

    #[test]
    fn check_time() {
        let time = std::time::SystemTime::now();
        let current_time = time
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("time went backwards")
            .as_secs();
        println!("hello time is {}", current_time);
    }

    #[test]
    fn accepts_every_valid_match_path() {
        let mut ordinary_match = FootballSettled::new([1; 32]);
        for status in [
            CFootballMatchPhase::FirstHalf,
            CFootballMatchPhase::HalfTime,
            CFootballMatchPhase::SecondHalf,
            CFootballMatchPhase::Finished,
        ] {
            ordinary_match.set_match_status(status).unwrap();
        }

        let mut extra_time_match = FootballSettled::new([2; 32]);
        for status in [
            CFootballMatchPhase::FirstHalf,
            CFootballMatchPhase::HalfTime,
            CFootballMatchPhase::SecondHalf,
            CFootballMatchPhase::ExtraTimeFirstHalf,
            CFootballMatchPhase::ExtraTimeBreak,
            CFootballMatchPhase::ExtraTimeSecondHalf,
            CFootballMatchPhase::Penalties,
            CFootballMatchPhase::Finished,
        ] {
            extra_time_match.set_match_status(status).unwrap();
        }
    }

    #[test]
    fn rejects_skipped_or_backwards_transitions() {
        let mut match_state = FootballSettled::new([3; 32]);

        assert!(
            match_state
                .set_match_status(CFootballMatchPhase::HalfTime)
                .is_err()
        );

        match_state.status = CFootballMatchPhase::FirstHalf;
        assert!(
            match_state
                .set_match_status(CFootballMatchPhase::SecondHalf)
                .is_err()
        );

        match_state.status = CFootballMatchPhase::SecondHalf;
        assert!(
            match_state
                .set_match_status(CFootballMatchPhase::Penalties)
                .is_err()
        );

        match_state.status = CFootballMatchPhase::Finished;
        assert!(
            match_state
                .set_match_status(CFootballMatchPhase::FirstHalf)
                .is_err()
        );
    }

    #[test]
    fn accepts_idempotent_status_updates() {
        let mut match_state = FootballSettled::new([4; 32]);
        match_state
            .set_match_status(CFootballMatchPhase::Scheduled)
            .unwrap();
        assert!(match_state.status == CFootballMatchPhase::Scheduled);
    }
}
