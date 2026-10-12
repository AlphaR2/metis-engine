//! What makes a market's YES outcome win, judged on the oracle's 90-minute score.
//!
//! A market is binary: outcome 0 is YES and outcome 1 is NO, and NO wins whenever YES doesn't.
//! The creator picks one of these conditions when the market is created and it can't change.
//! They're deliberately machine-checkable - there is no "the referee decides" - because
//! settlement has to be a pure function of what the oracle holds.
//!
//! Always judged on the regulation (90-minute) score, never the final one, so extra time and
//! penalties can't change a market that was about the 90 minutes.

pub const HOME_WIN: u8 = 0;
pub const AWAY_WIN: u8 = 1;
pub const DRAW: u8 = 2;
/// YES if total goals are MORE than `param` (so param 2 is "over 2.5").
pub const TOTAL_GOALS_OVER: u8 = 3;
pub const BOTH_TEAMS_SCORE: u8 = 4;
/// YES if the home side scores MORE than `param` goals ("Man U score more than 3").
pub const HOME_GOALS_OVER: u8 = 5;
/// YES if the away side scores MORE than `param` goals.
pub const AWAY_GOALS_OVER: u8 = 6;
/// YES if the home side wins by MORE than `param` goals (param 3 means a margin of 4 or more:
/// "Man U win by more than 3"). Param 0 is a plain home win, which has its own kind.
pub const HOME_WIN_BY_OVER: u8 = 7;
/// YES if the away side wins by MORE than `param` goals.
pub const AWAY_WIN_BY_OVER: u8 = 8;

/// Highest goal line a creator can pick. Two nibbles can carry at most 30 goals between them.
pub const MAX_GOAL_LINE: u8 = 29;

/// Highest line for a single side or a margin. One side's score is a nibble (at most 15), so a
/// line of 15 could never be beaten and a market on it would be decided before it started.
pub const MAX_TEAM_LINE: u8 = 14;

/// Whether (kind, param) describes a real condition. Conditions that take no parameter insist
/// it is zero, so two creators can't make "the same" market that hashes differently.
pub fn is_valid(kind: u8, param: u8) -> bool {
    match kind {
        HOME_WIN | AWAY_WIN | DRAW | BOTH_TEAMS_SCORE => param == 0,
        TOTAL_GOALS_OVER => param <= MAX_GOAL_LINE,
        HOME_GOALS_OVER | AWAY_GOALS_OVER | HOME_WIN_BY_OVER | AWAY_WIN_BY_OVER => param <= MAX_TEAM_LINE,
        _ => false,
    }
}

/// Whether YES wins on this regulation score. `None` for a condition that doesn't exist.
pub fn yes_wins(kind: u8, param: u8, home: u8, away: u8) -> Option<bool> {
    Some(match kind {
        HOME_WIN => home > away,
        AWAY_WIN => away > home,
        DRAW => home == away,
        TOTAL_GOALS_OVER => (home as u16 + away as u16) > param as u16,
        BOTH_TEAMS_SCORE => home > 0 && away > 0,
        HOME_GOALS_OVER => home > param,
        AWAY_GOALS_OVER => away > param,
        HOME_WIN_BY_OVER => (home as i16 - away as i16) > param as i16,
        AWAY_WIN_BY_OVER => (away as i16 - home as i16) > param as i16,
        _ => return None,
    })
}

/// The winning outcome index: 0 (YES) if the condition held, else 1 (NO).
pub fn winning_outcome(kind: u8, param: u8, home: u8, away: u8) -> Option<u8> {
    yes_wins(kind, param, home, away).map(|yes| if yes { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_condition_on_the_scores_that_matter() {
        // (kind, param, home, away, expected YES)
        let cases = [
            (HOME_WIN, 0, 2, 1, true),
            (HOME_WIN, 0, 1, 1, false),
            (HOME_WIN, 0, 0, 3, false),
            (AWAY_WIN, 0, 0, 3, true),
            (AWAY_WIN, 0, 2, 2, false),
            (DRAW, 0, 0, 0, true),
            (DRAW, 0, 2, 2, true),
            (DRAW, 0, 1, 0, false),
            (TOTAL_GOALS_OVER, 2, 2, 1, true),
            (TOTAL_GOALS_OVER, 2, 1, 1, false),
            (TOTAL_GOALS_OVER, 0, 1, 0, true),
            (TOTAL_GOALS_OVER, 0, 0, 0, false),
            (BOTH_TEAMS_SCORE, 0, 1, 1, true),
            (BOTH_TEAMS_SCORE, 0, 3, 0, false),
            (BOTH_TEAMS_SCORE, 0, 0, 0, false),
            (HOME_GOALS_OVER, 3, 4, 0, true),
            (HOME_GOALS_OVER, 3, 3, 0, false),
            (HOME_GOALS_OVER, 3, 0, 5, false),
            (AWAY_GOALS_OVER, 1, 0, 2, true),
            (AWAY_GOALS_OVER, 1, 5, 1, false),
            // "wins by more than 3" needs a margin of 4 or more.
            (HOME_WIN_BY_OVER, 3, 4, 0, true),
            (HOME_WIN_BY_OVER, 3, 3, 0, false),
            (HOME_WIN_BY_OVER, 3, 5, 1, true),
            (HOME_WIN_BY_OVER, 3, 0, 4, false),
            (AWAY_WIN_BY_OVER, 2, 0, 3, true),
            (AWAY_WIN_BY_OVER, 2, 1, 3, false),
            (AWAY_WIN_BY_OVER, 2, 4, 0, false),
        ];
        for (kind, param, h, a, expected) in cases {
            assert_eq!(yes_wins(kind, param, h, a), Some(expected), "kind {kind} param {param} score {h}-{a}");
        }
    }

    #[test]
    fn a_draw_is_no_for_a_win_market_never_a_third_outcome() {
        assert_eq!(winning_outcome(HOME_WIN, 0, 1, 1), Some(1));
        assert_eq!(winning_outcome(HOME_WIN, 0, 2, 1), Some(0));
    }

    #[test]
    fn the_goal_line_cannot_overflow() {
        // 15 + 15 = 30 goals is the most the oracle can hold; a line of 29 is the highest valid.
        assert_eq!(yes_wins(TOTAL_GOALS_OVER, MAX_GOAL_LINE, 15, 15), Some(true));
        assert_eq!(yes_wins(TOTAL_GOALS_OVER, MAX_GOAL_LINE, 15, 14), Some(false));
        assert!(!is_valid(TOTAL_GOALS_OVER, MAX_GOAL_LINE + 1));
    }

    #[test]
    fn unknown_conditions_and_stray_parameters_are_refused() {
        assert!(!is_valid(9, 0));
        assert!(!is_valid(255, 0));
        assert!(!is_valid(HOME_WIN, 1), "a win market takes no parameter");
        assert!(!is_valid(BOTH_TEAMS_SCORE, 3));
        assert_eq!(yes_wins(9, 0, 1, 0), None);
        for kind in [HOME_WIN, AWAY_WIN, DRAW, BOTH_TEAMS_SCORE] {
            assert!(is_valid(kind, 0));
        }
    }

    // A market whose answer is known before kickoff is not a market. Every line the program
    // accepts must leave both YES and NO reachable on some score the oracle can hold, and the
    // first line past the cap is refused.
    #[test]
    fn every_accepted_line_can_still_go_either_way() {
        for kind in [HOME_GOALS_OVER, AWAY_GOALS_OVER, HOME_WIN_BY_OVER, AWAY_WIN_BY_OVER] {
            for param in 0..=MAX_TEAM_LINE {
                assert!(is_valid(kind, param), "kind {kind} line {param} should be accepted");
                let (mut yes, mut no) = (false, false);
                for h in 0..=15u8 {
                    for a in 0..=15u8 {
                        match yes_wins(kind, param, h, a).unwrap() {
                            true => yes = true,
                            false => no = true,
                        }
                    }
                }
                assert!(yes && no, "kind {kind} line {param} is decided before kickoff");
            }
            assert!(!is_valid(kind, MAX_TEAM_LINE + 1), "kind {kind} line {} must be refused", MAX_TEAM_LINE + 1);
        }
    }

    // Whatever the score, exactly one of YES and NO wins: a market always pays someone.
    #[test]
    fn every_score_has_exactly_one_winner_for_every_condition() {
        for kind in [
            HOME_WIN, AWAY_WIN, DRAW, TOTAL_GOALS_OVER, BOTH_TEAMS_SCORE,
            HOME_GOALS_OVER, AWAY_GOALS_OVER, HOME_WIN_BY_OVER, AWAY_WIN_BY_OVER,
        ] {
            for h in 0..=15u8 {
                for a in 0..=15u8 {
                    let w = winning_outcome(kind, 2, h, a).unwrap();
                    assert!(w <= 1);
                }
            }
        }
    }
}
