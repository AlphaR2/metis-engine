use bytemuck::{CheckedBitPattern, NoUninit, Pod, Zeroable};
use pinocchio::error::ProgramError;
use shank::{ShankAccount, ShankType};

use crate::{
    constants::{
        FOOTBALL_SETTLED_DISCRIMINATOR, MAX_REPORTERS, OVERRIDE_WINDOW_SECS, REQUIRED_REPORTS,
        SCORE_NIBBLE_MASK, SCORE_NIBBLE_MAX,
    },
    error::OracleError,
    state::MarketKind,
};

/// What a reporter claims about the match right now. Two reporters must claim exactly the same
/// thing before it is applied.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Claim {
    pub status: CFootballMatchPhase,
    /// Packed: high nibble home, low nibble away.
    pub score: u8,
    /// Packed the same way. Only meaningful from the Penalties phase on.
    pub penalties: u8,
}

/// One reporter's latest claim, kept until a quorum applies it (then every slot is cleared so
/// the next change needs fresh agreement).
#[repr(C)]
#[derive(Default, Clone, Copy, Debug, Pod, Zeroable, ShankType)]
pub struct Report {
    pub reporter: [u8; 32],
    /// 0 = empty slot.
    pub used: u8,
    /// A CFootballMatchPhase as its byte (only ever written from the enum), kept as a plain
    /// byte so the whole slot is Pod.
    pub status: u8,
    pub score: u8,
    pub penalties: u8,
}

impl Report {
    #[inline]
    fn says(&self, claim: &Claim) -> bool {
        self.used != 0
            && self.status == claim.status as u8
            && self.score == claim.score
            && self.penalties == claim.penalties
    }
}

/// One football match. Created `Scheduled` with a kickoff time and nothing else; every later
/// change needs `REQUIRED_REPORTS` distinct reporters to agree, and once `Finished` is applied
/// the result is final (a two-root override within OVERRIDE_WINDOW_SECS is the only way out).
///
/// Every field is a byte or byte array, so the layout has no padding and alignment 1; the
/// discriminator stays the last 8 bytes, which is where metis-core's market-creation check
/// looks for it.
#[repr(C)]
#[derive(Default, Clone, Copy, NoUninit, CheckedBitPattern, ShankAccount)]
pub struct FootballSettled {
    pub market_kind: MarketKind,
    /// Immutable commitment to the provider fixture aliases for this event.
    /// Every Oracle account stores this at bytes 1..33, immediately after its
    /// one-byte market kind, so generic instructions need not decode
    /// sport-specific fields.
    pub fixture_merkle_root: [u8; 32],
    /// i64 LE, unix seconds. Fixed at creation.
    pub kickoff_ts: [u8; 8],
    /// i64 LE. Last time a quorum (or override) applied a change.
    pub updated_at_unix: [u8; 8],
    /// i64 LE. When the result was finalised or voided; 0 before that.
    pub finished_at: [u8; 8],
    // Shank needs a literal length here; the assert below ties it to MAX_REPORTERS.
    pub reports: [Report; 3],
    /// Live score, packed: high nibble home, low nibble away.
    pub score: u8,
    pub penalties: u8,
    /// The score at the end of normal time (90 minutes), frozen the moment the match leaves
    /// SecondHalf. Markets are judged on this, never on the final score, so extra time and
    /// penalties can't change a 90-minute market. Zero until then.
    pub regulation_score: u8,
    pub status: CFootballMatchPhase,
    /// 1 once the match is Finished and the result applied. Terminal.
    pub finalized: u8,
    /// 1 if two roots voided the result. Markets on a voided oracle cancel.
    pub voided: u8,
    /// 1 if two roots overrode what the reporters agreed (kept for transparency).
    pub overridden: u8,
    pub discriminator: [u8; 8],
}

const _: () = assert!(MAX_REPORTERS == 3, "FootballSettled.reports is declared with a literal 3");

// Every non-Scheduled variant is only ever constructed by tests today (real phase transitions
// are driven by the oracle keeper).
#[allow(dead_code)]
#[repr(u8)]
#[derive(Clone, Copy, Default, NoUninit, CheckedBitPattern, PartialEq, Eq, Debug, ShankType)]
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
    /// Re-sending the current phase is allowed so a score update inside a phase (a goal) and an
    /// idempotent retry both work.
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

/// What a two-root override does.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OverrideAction {
    /// Replace the result with this one and finalise it.
    SetResult = 0,
    /// Void the result; markets on it cancel and refund.
    Void = 1,
}

impl TryFrom<u8> for OverrideAction {
    type Error = ProgramError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::SetResult),
            1 => Ok(Self::Void),
            _ => Err(ProgramError::InvalidInstructionData),
        }
    }
}

#[allow(dead_code)]
impl FootballSettled {
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// A fresh fixture: Scheduled, scoreless, no reports. `now` is passed in (not read from the
    /// clock here) so the whole state machine is testable off-chain.
    pub fn new(fixture_merkle_root: [u8; 32], kickoff_ts: i64, now: i64) -> Self {
        Self {
            market_kind: MarketKind::Football,
            fixture_merkle_root,
            kickoff_ts: kickoff_ts.to_le_bytes(),
            updated_at_unix: now.to_le_bytes(),
            finished_at: 0i64.to_le_bytes(),
            reports: [Report::default(); MAX_REPORTERS],
            score: 0,
            penalties: 0,
            regulation_score: 0,
            status: CFootballMatchPhase::Scheduled,
            finalized: 0,
            voided: 0,
            overridden: 0,
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
    pub fn kickoff(&self) -> i64 {
        i64::from_le_bytes(self.kickoff_ts)
    }

    #[inline]
    pub fn finished(&self) -> i64 {
        i64::from_le_bytes(self.finished_at)
    }

    #[inline]
    pub fn is_finalized(&self) -> bool {
        self.finalized != 0
    }

    #[inline]
    pub fn is_voided(&self) -> bool {
        self.voided != 0
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
        (self.home_score(), self.away_score())
    }

    /// (home, away) at the end of normal time. Only meaningful once the match has left
    /// SecondHalf.
    #[inline]
    pub fn regulation(&self) -> (u8, u8) {
        (self.regulation_score >> 4, self.regulation_score & 0x0F)
    }

    #[inline]
    pub fn pack_score(home: u8, away: u8) -> u8 {
        (home.min(SCORE_NIBBLE_MAX) << 4) | (away.min(SCORE_NIBBLE_MAX) & SCORE_NIBBLE_MASK)
    }

    /// Whether `claim` could be true at `now`. Rejects the impossible outright so a typo or a
    /// lying reporter can't even be counted towards a quorum.
    fn check_claim(&self, claim: &Claim, now: i64) -> Result<(), ProgramError> {
        // Nothing but "Scheduled" can be true before kickoff.
        if claim.status != CFootballMatchPhase::Scheduled && now < self.kickoff() {
            return Err(OracleError::ResultBeforeKickoff.into());
        }
        if claim.status == CFootballMatchPhase::Scheduled && (claim.score != 0 || claim.penalties != 0) {
            return Err(OracleError::InvalidReport.into());
        }
        // A shoot-out only exists from the Penalties phase on.
        if (claim.status as u8) < (CFootballMatchPhase::Penalties as u8) && claim.penalties != 0 {
            return Err(OracleError::InvalidReport.into());
        }
        Ok(())
    }

    /// Records `reporter`'s claim, and applies it if `REQUIRED_REPORTS` distinct reporters now
    /// say the same thing. Returns whether it was applied.
    ///
    /// Takes the reporter's address as bytes; the caller has already proven (via the attested
    /// DelegatedAuthority account) that this signer is allowed to report.
    pub fn submit(&mut self, reporter: [u8; 32], claim: Claim, now: i64) -> Result<bool, ProgramError> {
        if self.is_voided() {
            return Err(OracleError::OracleVoided.into());
        }
        if self.is_finalized() {
            return Err(OracleError::OracleFinalized.into());
        }
        self.check_claim(&claim, now)?;

        // One slot per reporter: reuse theirs if they've reported before, else take a free one.
        let slot = match self.reports.iter().position(|r| r.used != 0 && r.reporter == reporter) {
            Some(existing) => existing,
            None => self
                .reports
                .iter()
                .position(|r| r.used == 0)
                .ok_or(OracleError::TooManyReporters)?,
        };
        self.reports[slot] = Report {
            reporter,
            used: 1,
            status: claim.status as u8,
            score: claim.score,
            penalties: claim.penalties,
        };

        let agreeing = self.reports.iter().filter(|r| r.says(&claim)).count();
        if agreeing < REQUIRED_REPORTS {
            return Ok(false);
        }

        self.apply(claim, now)?;
        Ok(true)
    }

    /// Applies an agreed claim. Rejects a transition the state machine doesn't allow, so even a
    /// full quorum can't skip or reverse a phase.
    fn apply(&mut self, claim: Claim, now: i64) -> Result<(), ProgramError> {
        if !self.status.can_transition_to(claim.status) {
            return Err(OracleError::InvalidStateTransition.into());
        }

        // The score at the moment normal time ends is frozen here, computed by the program
        // rather than reported, so it can't be mis-stated later.
        if self.status == CFootballMatchPhase::SecondHalf
            && matches!(
                claim.status,
                CFootballMatchPhase::Finished | CFootballMatchPhase::ExtraTimeFirstHalf
            )
        {
            self.regulation_score = claim.score;
        }

        self.status = claim.status;
        self.score = claim.score;
        self.penalties = claim.penalties;
        self.updated_at_unix = now.to_le_bytes();
        self.reports = [Report::default(); MAX_REPORTERS];

        if claim.status == CFootballMatchPhase::Finished {
            self.finalized = 1;
            self.finished_at = now.to_le_bytes();
        }
        Ok(())
    }

    /// A two-root override. Allowed until OVERRIDE_WINDOW_SECS after the result was finalised
    /// (or any time before it is), never after a void.
    pub fn apply_override(
        &mut self,
        action: OverrideAction,
        claim: Claim,
        regulation_score: u8,
        now: i64,
    ) -> Result<(), ProgramError> {
        if self.is_voided() {
            return Err(OracleError::OracleVoided.into());
        }
        if self.is_finalized() && now >= self.finished() + OVERRIDE_WINDOW_SECS {
            return Err(OracleError::OverrideWindowClosed.into());
        }

        match action {
            OverrideAction::SetResult => {
                if claim.status != CFootballMatchPhase::Finished {
                    return Err(OracleError::InvalidReport.into());
                }
                self.status = CFootballMatchPhase::Finished;
                self.score = claim.score;
                self.penalties = claim.penalties;
                self.regulation_score = regulation_score;
                self.finalized = 1;
            }
            OverrideAction::Void => {
                self.voided = 1;
            }
        }
        self.overridden = 1;
        // Restarts the clocks: markets get a fresh dispute window after an override.
        self.finished_at = now.to_le_bytes();
        self.updated_at_unix = now.to_le_bytes();
        self.reports = [Report::default(); MAX_REPORTERS];
        Ok(())
    }

    /// When this oracle may be closed: the grace period after it was finalised or voided, or -
    /// if it never was - after kickoff.
    #[inline]
    pub fn closable_at(&self, grace_secs: i64) -> i64 {
        let anchor = if self.finished() != 0 { self.finished() } else { self.kickoff() };
        anchor + grace_secs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::CLOSE_GRACE_SECS;

    const KICKOFF: i64 = 10_000;
    const A: [u8; 32] = [1; 32];
    const B: [u8; 32] = [2; 32];
    const C: [u8; 32] = [3; 32];
    const D: [u8; 32] = [4; 32];

    fn fresh() -> FootballSettled {
        FootballSettled::new([9; 32], KICKOFF, 0)
    }

    fn claim(status: CFootballMatchPhase, home: u8, away: u8) -> Claim {
        Claim { status, score: FootballSettled::pack_score(home, away), penalties: 0 }
    }

    /// Two reporters agree: applies on the second.
    fn agree(m: &mut FootballSettled, c: Claim, now: i64) {
        assert!(!m.submit(A, c, now).unwrap(), "one report must not apply");
        assert!(m.submit(B, c, now).unwrap(), "the second matching report applies");
    }

    /// Walks a match to the end of second half with the given score.
    fn to_second_half(m: &mut FootballSettled, home: u8, away: u8) {
        let t = KICKOFF + 1;
        agree(m, claim(CFootballMatchPhase::FirstHalf, 0, 0), t);
        agree(m, claim(CFootballMatchPhase::HalfTime, 0, 0), t);
        agree(m, claim(CFootballMatchPhase::SecondHalf, home, away), t);
    }

    #[test]
    fn accepts_every_valid_match_path() {
        let mut ordinary = fresh();
        to_second_half(&mut ordinary, 2, 1);
        agree(&mut ordinary, claim(CFootballMatchPhase::Finished, 2, 1), KICKOFF + 7_000);
        assert!(ordinary.is_finalized());

        let mut extra_time = fresh();
        to_second_half(&mut extra_time, 1, 1);
        for (phase, h, a) in [
            (CFootballMatchPhase::ExtraTimeFirstHalf, 1, 1),
            (CFootballMatchPhase::ExtraTimeBreak, 1, 1),
            (CFootballMatchPhase::ExtraTimeSecondHalf, 2, 1),
            (CFootballMatchPhase::Penalties, 2, 1),
        ] {
            agree(&mut extra_time, claim(phase, h, a), KICKOFF + 8_000);
        }
        let mut done = claim(CFootballMatchPhase::Finished, 2, 1);
        done.penalties = FootballSettled::pack_score(4, 3);
        agree(&mut extra_time, done, KICKOFF + 9_000);
        assert!(extra_time.is_finalized());
    }

    #[test]
    fn rejects_skipped_or_backwards_transitions() {
        let mut m = fresh();
        // Both reporters agree on a jump the state machine forbids: the second report errors
        // rather than applying, and nothing changes.
        assert!(m.submit(A, claim(CFootballMatchPhase::HalfTime, 0, 0), KICKOFF + 1).is_ok());
        assert!(m.submit(B, claim(CFootballMatchPhase::HalfTime, 0, 0), KICKOFF + 1).is_err());
        assert_eq!(m.status, CFootballMatchPhase::Scheduled);
    }

    #[test]
    fn one_reporter_can_never_move_the_state_alone() {
        let mut m = fresh();
        // The same reporter repeating itself fills one slot, so it never reaches two.
        for _ in 0..5 {
            assert!(!m.submit(A, claim(CFootballMatchPhase::FirstHalf, 0, 0), KICKOFF + 1).unwrap());
        }
        assert_eq!(m.status, CFootballMatchPhase::Scheduled);
    }

    #[test]
    fn disagreeing_reporters_apply_nothing() {
        let mut m = fresh();
        to_second_half(&mut m, 0, 0);
        assert!(!m.submit(A, claim(CFootballMatchPhase::SecondHalf, 1, 0), KICKOFF + 2).unwrap());
        assert!(!m.submit(B, claim(CFootballMatchPhase::SecondHalf, 0, 1), KICKOFF + 2).unwrap());
        assert_eq!(m.get_score(), (0, 0));
        // A third reporter siding with one of them is what breaks the tie.
        assert!(m.submit(C, claim(CFootballMatchPhase::SecondHalf, 1, 0), KICKOFF + 2).unwrap());
        assert_eq!(m.get_score(), (1, 0));
    }

    #[test]
    fn a_reporter_can_change_its_mind_and_the_slot_is_reused() {
        let mut m = fresh();
        to_second_half(&mut m, 0, 0);
        m.submit(A, claim(CFootballMatchPhase::SecondHalf, 5, 5), KICKOFF + 2).unwrap();
        m.submit(A, claim(CFootballMatchPhase::SecondHalf, 1, 0), KICKOFF + 2).unwrap();
        assert!(m.submit(B, claim(CFootballMatchPhase::SecondHalf, 1, 0), KICKOFF + 2).unwrap());
    }

    #[test]
    fn a_fourth_reporter_is_turned_away_while_three_slots_are_held() {
        let mut m = fresh();
        let t = KICKOFF + 1;
        m.submit(A, claim(CFootballMatchPhase::FirstHalf, 0, 0), t).unwrap();
        // B and C disagree with A and with each other, so all three slots stay held.
        m.submit(B, claim(CFootballMatchPhase::Scheduled, 0, 0), t).unwrap();
        m.submit(C, claim(CFootballMatchPhase::FirstHalf, 1, 0), t).unwrap();
        assert!(m.submit(D, claim(CFootballMatchPhase::FirstHalf, 0, 0), t).is_err());
    }

    #[test]
    fn nothing_but_scheduled_can_be_reported_before_kickoff() {
        let mut m = fresh();
        assert!(m.submit(A, claim(CFootballMatchPhase::FirstHalf, 0, 0), KICKOFF - 1).is_err());
        assert!(m.submit(A, claim(CFootballMatchPhase::FirstHalf, 0, 0), KICKOFF).is_ok());
    }

    #[test]
    fn impossible_claims_never_count() {
        let mut m = fresh();
        let scoreless = Claim { status: CFootballMatchPhase::Scheduled, score: 0x10, penalties: 0 };
        assert!(m.submit(A, scoreless, 0).is_err(), "a score before kickoff");
        let early_penalties =
            Claim { status: CFootballMatchPhase::FirstHalf, score: 0, penalties: 0x43 };
        assert!(m.submit(A, early_penalties, KICKOFF + 1).is_err(), "penalties in the first half");
    }

    #[test]
    fn regulation_score_freezes_when_normal_time_ends_and_survives_extra_time() {
        let mut m = fresh();
        to_second_half(&mut m, 1, 1);
        assert_eq!(m.regulation(), (0, 0), "unset while the match is live");
        agree(&mut m, claim(CFootballMatchPhase::ExtraTimeFirstHalf, 1, 1), KICKOFF + 6_000);
        assert_eq!(m.regulation(), (1, 1));
        agree(&mut m, claim(CFootballMatchPhase::ExtraTimeBreak, 1, 1), KICKOFF + 7_000);
        agree(&mut m, claim(CFootballMatchPhase::ExtraTimeSecondHalf, 3, 1), KICKOFF + 7_100);
        agree(&mut m, claim(CFootballMatchPhase::Finished, 3, 1), KICKOFF + 8_000);
        assert_eq!(m.get_score(), (3, 1), "the final score includes extra time");
        assert_eq!(m.regulation(), (1, 1), "the 90-minute score does not");
    }

    #[test]
    fn a_ninety_minute_finish_freezes_regulation_at_the_final_score() {
        let mut m = fresh();
        to_second_half(&mut m, 2, 0);
        agree(&mut m, claim(CFootballMatchPhase::Finished, 2, 0), KICKOFF + 6_000);
        assert_eq!(m.regulation(), (2, 0));
    }

    #[test]
    fn a_finalised_result_cannot_be_changed_by_reporters() {
        let mut m = fresh();
        to_second_half(&mut m, 2, 1);
        agree(&mut m, claim(CFootballMatchPhase::Finished, 2, 1), KICKOFF + 6_000);
        assert!(m.submit(A, claim(CFootballMatchPhase::Finished, 0, 9), KICKOFF + 6_001).is_err());
        assert_eq!(m.get_score(), (2, 1));
    }

    #[test]
    fn override_replaces_a_result_inside_the_window_and_restarts_the_clock() {
        let mut m = fresh();
        to_second_half(&mut m, 2, 1);
        let finished_at = KICKOFF + 6_000;
        agree(&mut m, claim(CFootballMatchPhase::Finished, 2, 1), finished_at);

        let fixed = claim(CFootballMatchPhase::Finished, 1, 2);
        let later = finished_at + OVERRIDE_WINDOW_SECS - 1;
        m.apply_override(OverrideAction::SetResult, fixed, fixed.score, later).unwrap();
        assert_eq!(m.get_score(), (1, 2));
        assert_eq!(m.regulation(), (1, 2));
        assert!(m.overridden != 0 && m.is_finalized());
        assert_eq!(m.finished(), later, "markets get a fresh dispute window");
    }

    #[test]
    fn override_is_refused_once_the_window_has_closed() {
        let mut m = fresh();
        to_second_half(&mut m, 2, 1);
        let finished_at = KICKOFF + 6_000;
        agree(&mut m, claim(CFootballMatchPhase::Finished, 2, 1), finished_at);
        let c = claim(CFootballMatchPhase::Finished, 0, 0);
        assert!(m
            .apply_override(OverrideAction::SetResult, c, c.score, finished_at + OVERRIDE_WINDOW_SECS)
            .is_err());
        assert!(m
            .apply_override(OverrideAction::Void, c, 0, finished_at + OVERRIDE_WINDOW_SECS)
            .is_err());
    }

    #[test]
    fn void_is_terminal() {
        let mut m = fresh();
        m.apply_override(OverrideAction::Void, claim(CFootballMatchPhase::Scheduled, 0, 0), 0, KICKOFF).unwrap();
        assert!(m.is_voided());
        assert!(m.submit(A, claim(CFootballMatchPhase::FirstHalf, 0, 0), KICKOFF + 1).is_err());
        let c = claim(CFootballMatchPhase::Finished, 1, 0);
        assert!(m.apply_override(OverrideAction::SetResult, c, c.score, KICKOFF + 2).is_err());
    }

    #[test]
    fn an_override_can_only_set_a_finished_result() {
        let mut m = fresh();
        let live = claim(CFootballMatchPhase::SecondHalf, 1, 0);
        assert!(m.apply_override(OverrideAction::SetResult, live, live.score, KICKOFF + 1).is_err());
    }

    #[test]
    fn closable_only_after_the_grace_period_from_the_last_event() {
        let mut m = fresh();
        assert_eq!(m.closable_at(CLOSE_GRACE_SECS), KICKOFF + CLOSE_GRACE_SECS, "never started: from kickoff");
        to_second_half(&mut m, 1, 0);
        agree(&mut m, claim(CFootballMatchPhase::Finished, 1, 0), KICKOFF + 6_000);
        assert_eq!(m.closable_at(CLOSE_GRACE_SECS), KICKOFF + 6_000 + CLOSE_GRACE_SECS);
    }

    #[test]
    fn layout_keeps_the_header_and_trailing_discriminator_other_programs_read() {
        let m = fresh();
        let bytes = m.as_bytes();
        assert_eq!(bytes.len(), FootballSettled::LEN);
        assert_eq!(&bytes[1..33], &[9u8; 32], "fixture root stays at bytes 1..33");
        assert_eq!(&bytes[bytes.len() - 8..], &FOOTBALL_SETTLED_DISCRIMINATOR, "discriminator stays last");
        assert_eq!(core::mem::align_of::<FootballSettled>(), 1);
        println!("FootballSettled is {} bytes", FootballSettled::LEN);
    }
}
