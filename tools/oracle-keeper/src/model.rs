//! What a provider told us, in the oracle's own vocabulary.

pub use oracle::instructions::football::{CFootballMatchPhase as Phase, Claim, FootballSettled};

/// (home, away) goals.
pub type Goals = (u8, u8);

/// One provider's view of one match at one moment, already translated to the oracle's phases.
///
/// Providers disagree on how much they know (some can't tell which half of extra time it is),
/// so everything beyond `phase` and `score` is optional. A missing value makes the planner wait
/// rather than guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub source: &'static str,
    pub phase: Phase,
    /// Goals now, including extra time, excluding any shoot-out.
    pub score: Goals,
    /// The score when normal time (90 minutes) ended. `None` until the provider says.
    pub regulation: Option<Goals>,
    /// Shoot-out score, once there is one.
    pub penalties: Option<Goals>,
    pub went_to_extra_time: bool,
    pub went_to_penalties: bool,
}

impl Observation {
    pub fn pack(goals: Goals) -> u8 {
        FootballSettled::pack_score(goals.0, goals.1)
    }
}
