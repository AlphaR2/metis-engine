//! Decides what one keeper should claim next, given what the chain says and what its provider
//! says. Pure and synchronous, so every rule is a unit test.
//!
//! The oracle only accepts one legal phase step at a time and only applies a claim once two
//! reporters send exactly the same thing. So a keeper never jumps to what it sees: it claims the
//! next step along the legal path, carrying forward numbers the chain already holds. Two keepers
//! watching the same match then produce identical claims for every step and the quorum forms on
//! its own, even if one of them was offline while a phase came and went.

use crate::model::{Claim, FootballSettled, Observation, Phase};

/// The phase after `cur` on the way to what the provider reports.
fn next_step(cur: Phase, obs: &Observation) -> Option<Phase> {
    use Phase::*;
    let target = obs.phase;
    Some(match cur {
        Scheduled => FirstHalf,
        FirstHalf => HalfTime,
        HalfTime => SecondHalf,
        SecondHalf => {
            if obs.went_to_extra_time || matches!(target, ExtraTimeFirstHalf | ExtraTimeBreak | ExtraTimeSecondHalf | Penalties) {
                ExtraTimeFirstHalf
            } else {
                Finished
            }
        }
        ExtraTimeFirstHalf => ExtraTimeBreak,
        ExtraTimeBreak => ExtraTimeSecondHalf,
        ExtraTimeSecondHalf => {
            if obs.went_to_penalties || target == Penalties {
                Penalties
            } else {
                Finished
            }
        }
        Penalties => Finished,
        Finished => return None,
    })
}

/// Why a keeper is holding back, for the log.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Send this claim.
    Claim(Claim),
    /// Nothing to say: the chain already matches what the provider reports.
    UpToDate,
    /// The result is final (or voided); there is nothing left for a reporter to do.
    Settled,
    /// The provider isn't telling us enough yet (e.g. no 90-minute score). Better to wait than
    /// to guess a number two keepers might guess differently.
    Waiting(&'static str),
}

pub fn plan(chain: &FootballSettled, obs: &Observation) -> Plan {
    if chain.is_finalized() || chain.is_voided() {
        return Plan::Settled;
    }

    let cur = chain.status;
    let ahead = (obs.phase as u8) > (cur as u8);

    if !ahead {
        // Same phase (or the provider is behind the chain): only a live score change is news.
        let live = !matches!(cur, Phase::Scheduled | Phase::Finished);
        if obs.phase == cur && live {
            let penalties = if cur == Phase::Penalties { obs.penalties.map(Observation::pack) } else { None };
            if Observation::pack(obs.score) != chain.score
                || penalties.is_some_and(|p| p != chain.penalties)
            {
                return Plan::Claim(Claim {
                    status: cur,
                    score: Observation::pack(obs.score),
                    penalties: penalties.unwrap_or(chain.penalties),
                });
            }
        }
        return Plan::UpToDate;
    }

    let Some(next) = next_step(cur, obs) else { return Plan::Settled };

    let (score, penalties) = match (cur, next) {
        // Leaving normal time for extra time: the claim carries the 90-minute score, which the
        // oracle freezes as the regulation score. If the provider can't say, don't guess.
        (Phase::SecondHalf, Phase::ExtraTimeFirstHalf) => match obs.regulation {
            Some(r) => (Observation::pack(r), 0),
            None => return Plan::Waiting("provider has not given the 90-minute score"),
        },
        // The match is over: claim the final numbers.
        (_, Phase::Finished) => {
            let pens = if obs.went_to_penalties {
                match obs.penalties {
                    Some(p) => Observation::pack(p),
                    None => return Plan::Waiting("provider has not given the shoot-out score"),
                }
            } else {
                0
            };
            (Observation::pack(obs.score), pens)
        }
        // Any other step: keep the score the chain holds. Never invent one for a phase we're
        // only passing through.
        _ => (chain.score, 0),
    };

    Plan::Claim(Claim { status: next, score, penalties })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Goals;

    const KICKOFF: i64 = 1_000;

    fn chain(phase: Phase, score: Goals) -> FootballSettled {
        let mut c = FootballSettled::new([1; 32], KICKOFF, 0);
        c.status = phase;
        c.score = Observation::pack(score);
        c
    }

    fn obs(phase: Phase, score: Goals) -> Observation {
        Observation {
            source: "test",
            phase,
            score,
            regulation: None,
            penalties: None,
            went_to_extra_time: false,
            went_to_penalties: false,
        }
    }

    fn claim(p: Plan) -> Claim {
        match p {
            Plan::Claim(c) => c,
            other => panic!("expected a claim, got {other:?}"),
        }
    }

    #[test]
    fn walks_one_legal_step_at_a_time_even_when_the_provider_is_far_ahead() {
        // The provider says the match is over, but the chain is still Scheduled: first step only.
        let mut o = obs(Phase::Finished, (2, 1));
        o.regulation = Some((2, 1));
        let c = claim(plan(&chain(Phase::Scheduled, (0, 0)), &o));
        assert_eq!((c.status, c.score), (Phase::FirstHalf, 0), "no score invented for a passing phase");
    }

    #[test]
    fn two_keepers_with_different_views_make_the_same_claim_for_a_passing_phase() {
        let c = chain(Phase::FirstHalf, (0, 0));
        let a = claim(plan(&c, &obs(Phase::SecondHalf, (1, 0))));
        let b = claim(plan(&c, &obs(Phase::Finished, (2, 1))));
        assert_eq!(a, b, "both carry the chain's score into HalfTime, so the quorum can form");
    }

    #[test]
    fn a_live_score_change_in_the_same_phase_is_claimed() {
        let c = claim(plan(&chain(Phase::SecondHalf, (0, 0)), &obs(Phase::SecondHalf, (1, 0))));
        assert_eq!((c.status, c.score), (Phase::SecondHalf, Observation::pack((1, 0))));
        assert_eq!(plan(&chain(Phase::SecondHalf, (1, 0)), &obs(Phase::SecondHalf, (1, 0))), Plan::UpToDate);
    }

    #[test]
    fn a_provider_behind_the_chain_changes_nothing() {
        assert_eq!(plan(&chain(Phase::SecondHalf, (1, 0)), &obs(Phase::FirstHalf, (0, 0))), Plan::UpToDate);
    }

    #[test]
    fn a_ninety_minute_finish_claims_the_final_score() {
        let c = claim(plan(&chain(Phase::SecondHalf, (1, 0)), &obs(Phase::Finished, (2, 0))));
        assert_eq!((c.status, c.score, c.penalties), (Phase::Finished, Observation::pack((2, 0)), 0));
    }

    #[test]
    fn extra_time_leaves_normal_time_with_the_ninety_minute_score() {
        let mut o = obs(Phase::Finished, (3, 1));
        o.went_to_extra_time = true;
        o.regulation = Some((1, 1));
        let c = claim(plan(&chain(Phase::SecondHalf, (1, 1)), &o));
        assert_eq!((c.status, c.score), (Phase::ExtraTimeFirstHalf, Observation::pack((1, 1))));
    }

    #[test]
    fn it_waits_rather_than_guess_when_the_ninety_minute_score_is_missing() {
        let mut o = obs(Phase::Finished, (3, 1));
        o.went_to_extra_time = true;
        assert!(matches!(plan(&chain(Phase::SecondHalf, (1, 1)), &o), Plan::Waiting(_)));
    }

    #[test]
    fn a_shootout_is_walked_and_finished_with_both_scores() {
        let mut o = obs(Phase::Finished, (1, 1));
        o.went_to_extra_time = true;
        o.went_to_penalties = true;
        o.regulation = Some((1, 1));
        o.penalties = Some((4, 3));
        let walk = [
            (Phase::SecondHalf, Phase::ExtraTimeFirstHalf),
            (Phase::ExtraTimeFirstHalf, Phase::ExtraTimeBreak),
            (Phase::ExtraTimeBreak, Phase::ExtraTimeSecondHalf),
            (Phase::ExtraTimeSecondHalf, Phase::Penalties),
        ];
        for (from, to) in walk {
            assert_eq!(claim(plan(&chain(from, (1, 1)), &o)).status, to);
        }
        let last = claim(plan(&chain(Phase::Penalties, (1, 1)), &o));
        assert_eq!((last.status, last.penalties), (Phase::Finished, Observation::pack((4, 3))));
    }

    #[test]
    fn nothing_to_do_once_the_result_is_final_or_voided() {
        let mut done = chain(Phase::Finished, (2, 1));
        done.finalized = 1;
        assert_eq!(plan(&done, &obs(Phase::Finished, (9, 9))), Plan::Settled);
        let mut voided = chain(Phase::Scheduled, (0, 0));
        voided.voided = 1;
        assert_eq!(plan(&voided, &obs(Phase::FirstHalf, (0, 0))), Plan::Settled);
    }

    #[test]
    fn scheduled_matches_stay_quiet() {
        assert_eq!(plan(&chain(Phase::Scheduled, (0, 0)), &obs(Phase::Scheduled, (0, 0))), Plan::UpToDate);
    }
}
