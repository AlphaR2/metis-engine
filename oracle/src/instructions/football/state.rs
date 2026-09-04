use bytemuck::{CheckedBitPattern, NoUninit, Pod, Zeroable};
// use pack1::U32LE;
use pinocchio::ProgramResult;

use crate::{errors::OracleError, state::MarketKind};

#[repr(C)]
#[derive(Default, Clone, Copy, NoUninit, CheckedBitPattern)]
pub struct FootballSettled {
    pub market_kind: MarketKind,
    pub match_id: [u8; 4],
    pub score: u8, // this is a bitpack - higher 4 bits is home, lower 4 bits is away
    pub penalties: u8, // this is a bitpack - higher 4 bits is home, lower 4 bits is away
    pub status: CFootballMatchPhase, // 1 byte
                   // _p: u8,
                   // pub match_id: U32LE, // [u8; 4],
}

#[allow(warnings)]
#[repr(u8)]
#[derive(Clone, Copy, Default, NoUninit, CheckedBitPattern, PartialEq)]
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

#[allow(unused)]
impl FootballSettled {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(match_id: [u8; 4]) -> Self {
        Self {
            market_kind: MarketKind::Football,
            match_id,
            score: 0x00,
            penalties: 0x00,
            status: CFootballMatchPhase::Scheduled,
        }
    }

    // #[inline]
    // pub fn to_bytes(&self) -> &[u8; Self::LEN] {
    //     let bytes = bytemuck::cast_ref(self);
    //     bytes
    // }

    #[inline]
    pub fn to_bytes(&self) -> &[u8; Self::LEN] {
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
        let home_score = home.min(15);
        let away_score = away.min(15);
        // prevent overflow bleeding
        self.score = ((home & 0x0F) << 4) | (away & 0x0F);
    }

    #[inline]
    pub fn set_penalties(&mut self, home: u8, away: u8) {
        let home_score = home.min(15);
        let away_score = away.min(15);
        // prevent overflow bleeding
        self.penalties = ((home & 0x0F) << 4) | (away & 0x0F);
    }

    #[inline]
    pub fn set_match_status(&mut self, status: CFootballMatchPhase) -> ProgramResult {
        if !self.status.can_transition_to(status) {
            return Err(OracleError::InvalidStateTransition.into());
        }

        self.status = status;
        Ok(())
    }

    // AND
    // 0000_1111
    // 0000_0011 == 3
    // ||
    // ||
    // 0000_0011  << 4 is 0011_0000 right?
    // 0011_0000

    // AND
    // 0000_1111
    // 0000_0010 == 2
    // ||
    // ||
    // 0000_0010

    // so 0011_0000 | 000_0010
    // ||
    // 0011_0010

    pub fn set_score(&mut self, home_score: u8, away_score: u8) {
        let full_bytes = 0xFF; // or remove this entirely
        // check that home max score is 15
        // check that away max score is 15
        let _home_score = home_score.min(15);
        let _away_score = away_score.min(15);
        let home_ = (full_bytes & _home_score) << 4;
        let away_ = (full_bytes & _home_score);
        let score = home_ | away_;
        self.score = score
    }
}

#[cfg(test)]
mod tests {
    use super::{CFootballMatchPhase, FootballSettled};

    #[test]
    fn accepts_every_valid_match_path() {
        let mut ordinary_match = FootballSettled::new([1; 4]);
        for status in [
            CFootballMatchPhase::FirstHalf,
            CFootballMatchPhase::HalfTime,
            CFootballMatchPhase::SecondHalf,
            CFootballMatchPhase::Finished,
        ] {
            ordinary_match.set_match_status(status).unwrap();
        }

        let mut extra_time_match = FootballSettled::new([2; 4]);
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
        let mut match_state = FootballSettled::new([3; 4]);

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
        let mut match_state = FootballSettled::new([4; 4]);
        match_state
            .set_match_status(CFootballMatchPhase::Scheduled)
            .unwrap();
        assert!(match_state.status == CFootballMatchPhase::Scheduled);
    }
}

// #[repr(C)]
// #[derive(Pod, Zeroable, Default, Clone, Copy)]
// pub struct FootballSettledMatchPhase(u8);

// #[allow(warnings)]
// impl FootballSettledMatchPhase {
//     pub const Scheduled: FootballSettledMatchPhase = FootballSettledMatchPhase(0);
//     pub const FirstHalf: FootballSettledMatchPhase = FootballSettledMatchPhase(1);
//     pub const HalfTime: FootballSettledMatchPhase = FootballSettledMatchPhase(2);
//     pub const SecondHalf: FootballSettledMatchPhase = FootballSettledMatchPhase(3);
//     pub const ExtraTimeFirstHalf: FootballSettledMatchPhase = FootballSettledMatchPhase(4);
//     pub const ExtraTimeBreak: FootballSettledMatchPhase = FootballSettledMatchPhase(5);
//     pub const ExtraTimeSecondHalf: FootballSettledMatchPhase = FootballSettledMatchPhase(6);
//     pub const Penalties: FootballSettledMatchPhase = FootballSettledMatchPhase(7);
//     pub const Finished: FootballSettledMatchPhase = FootballSettledMatchPhase(8);
// }
