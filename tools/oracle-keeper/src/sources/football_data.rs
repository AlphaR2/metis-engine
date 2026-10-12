//! football-data.org, API v4: `GET /v4/matches/{id}` with the `X-Auth-Token` header.
//!
//! Field names follow the provider's public documentation; they have not yet been exercised
//! against the live API (that needs a key), only against the captured-shape samples in the tests
//! below. Known limits of this provider: it can't tell the two halves of extra time apart, and it
//! reports `regularTime` only for matches that went past 90 minutes - so a match that went to
//! extra time without it makes the planner wait instead of guessing the 90-minute score.

use async_trait::async_trait;
use serde_json::Value;

use super::{ScoreSource, SourceError, goals};
use crate::model::{Observation, Phase};

pub struct FootballData {
    client: reqwest::Client,
    base_url: String,
    token: String,
}

impl FootballData {
    pub fn new(token: String) -> Self {
        Self::with_base_url("https://api.football-data.org/v4".into(), token)
    }

    pub fn with_base_url(base_url: String, token: String) -> Self {
        Self { client: reqwest::Client::new(), base_url, token }
    }
}

#[async_trait]
impl ScoreSource for FootballData {
    fn id(&self) -> &'static str {
        "football-data"
    }

    async fn observe(&self, external_id: &str) -> Result<Observation, SourceError> {
        let url = format!("{}/matches/{}", self.base_url.trim_end_matches('/'), external_id);
        let body: Value = self
            .client
            .get(url)
            .header("X-Auth-Token", &self.token)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| SourceError::Http(e.to_string()))?
            .json()
            .await
            .map_err(|e| SourceError::Http(e.to_string()))?;
        parse(&body)
    }
}

pub fn parse(m: &Value) -> Result<Observation, SourceError> {
    let status = m.get("status").and_then(Value::as_str).ok_or(SourceError::Parse("no status"))?;
    let score = m.get("score").ok_or(SourceError::Parse("no score"))?;
    let duration = score.get("duration").and_then(Value::as_str).unwrap_or("REGULAR");

    let went_to_extra_time = matches!(duration, "EXTRA_TIME" | "PENALTY_SHOOTOUT");
    let went_to_penalties = duration == "PENALTY_SHOOTOUT";

    let phase = match status {
        "SCHEDULED" | "TIMED" => Phase::Scheduled,
        "IN_PLAY" => match duration {
            "PENALTY_SHOOTOUT" => Phase::Penalties,
            // Can't tell the halves of extra time apart; the later one never holds a result back.
            "EXTRA_TIME" => Phase::ExtraTimeSecondHalf,
            // Half-time is filled in once the break has happened, so its presence means second half.
            _ if goals(score, &["halfTime", "home"], &["halfTime", "away"]).is_some() => Phase::SecondHalf,
            _ => Phase::FirstHalf,
        },
        "PAUSED" => if duration == "EXTRA_TIME" { Phase::ExtraTimeBreak } else { Phase::HalfTime },
        "FINISHED" => Phase::Finished,
        other => return Err(SourceError::Abnormal(other.to_string())),
    };

    let full = goals(score, &["fullTime", "home"], &["fullTime", "away"]);
    let regular = goals(score, &["regularTime", "home"], &["regularTime", "away"]);
    let extra = goals(score, &["extraTime", "home"], &["extraTime", "away"]);
    let penalties = goals(score, &["penalties", "home"], &["penalties", "away"]);

    // Goals so far, including extra time but never the shoot-out. Where the provider splits the
    // match into regular + extra time, add them rather than trust `fullTime`, whose handling of
    // a shoot-out has varied.
    let total = match (regular, extra) {
        (Some(r), Some(e)) if (r.0 + e.0) <= 15 && (r.1 + e.1) <= 15 => (r.0 + e.0, r.1 + e.1),
        _ => full.unwrap_or((0, 0)),
    };

    Ok(Observation {
        source: "football-data",
        phase,
        score: total,
        // A match that ended in 90 minutes: the final score IS the 90-minute score. Otherwise only
        // `regularTime` will do.
        regulation: if went_to_extra_time { regular } else { full },
        penalties,
        went_to_extra_time,
        went_to_penalties,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_scheduled_match() {
        let o = parse(&json!({"status": "TIMED", "score": {"duration": "REGULAR", "fullTime": {"home": null, "away": null}}})).unwrap();
        assert_eq!((o.phase, o.score), (Phase::Scheduled, (0, 0)));
    }

    #[test]
    fn first_half_then_second_half_by_the_half_time_score() {
        let first = parse(&json!({"status": "IN_PLAY", "score": {"duration": "REGULAR",
            "fullTime": {"home": 1, "away": 0}, "halfTime": {"home": null, "away": null}}})).unwrap();
        assert_eq!((first.phase, first.score), (Phase::FirstHalf, (1, 0)));

        let second = parse(&json!({"status": "IN_PLAY", "score": {"duration": "REGULAR",
            "fullTime": {"home": 2, "away": 1}, "halfTime": {"home": 1, "away": 0}}})).unwrap();
        assert_eq!((second.phase, second.score), (Phase::SecondHalf, (2, 1)));
    }

    #[test]
    fn half_time() {
        let o = parse(&json!({"status": "PAUSED", "score": {"duration": "REGULAR",
            "fullTime": {"home": 1, "away": 1}, "halfTime": {"home": 1, "away": 1}}})).unwrap();
        assert_eq!(o.phase, Phase::HalfTime);
    }

    #[test]
    fn a_ninety_minute_finish_has_its_final_score_as_the_regulation_score() {
        let o = parse(&json!({"status": "FINISHED", "score": {"winner": "HOME_TEAM", "duration": "REGULAR",
            "fullTime": {"home": 2, "away": 1}, "halfTime": {"home": 1, "away": 0}}})).unwrap();
        assert_eq!((o.phase, o.score, o.regulation), (Phase::Finished, (2, 1), Some((2, 1))));
        assert!(!o.went_to_extra_time && !o.went_to_penalties);
    }

    #[test]
    fn extra_time_uses_regular_time_for_the_ninety_minute_score() {
        let o = parse(&json!({"status": "FINISHED", "score": {"duration": "EXTRA_TIME",
            "fullTime": {"home": 3, "away": 1},
            "regularTime": {"home": 1, "away": 1}, "extraTime": {"home": 2, "away": 0}}})).unwrap();
        assert_eq!((o.score, o.regulation), ((3, 1), Some((1, 1))));
        assert!(o.went_to_extra_time && !o.went_to_penalties);
    }

    #[test]
    fn a_shootout_is_not_counted_as_goals() {
        let o = parse(&json!({"status": "FINISHED", "score": {"duration": "PENALTY_SHOOTOUT",
            "fullTime": {"home": 6, "away": 5},
            "regularTime": {"home": 1, "away": 1}, "extraTime": {"home": 0, "away": 0},
            "penalties": {"home": 5, "away": 4}}})).unwrap();
        assert_eq!(o.score, (1, 1), "the shoot-out must not leak into the score");
        assert_eq!((o.regulation, o.penalties), (Some((1, 1)), Some((5, 4))));
        assert!(o.went_to_penalties);
    }

    #[test]
    fn extra_time_without_regular_time_leaves_the_regulation_score_unknown() {
        let o = parse(&json!({"status": "FINISHED", "score": {"duration": "EXTRA_TIME",
            "fullTime": {"home": 3, "away": 1}}})).unwrap();
        assert_eq!(o.regulation, None, "the planner will wait instead of guessing");
    }

    #[test]
    fn matches_that_are_not_playing_normally_are_refused() {
        for status in ["POSTPONED", "SUSPENDED", "CANCELLED", "AWARDED"] {
            let r = parse(&json!({"status": status, "score": {"fullTime": {"home": null, "away": null}}}));
            assert!(matches!(r, Err(SourceError::Abnormal(_))), "{status}");
        }
    }

    #[test]
    fn a_score_too_big_for_the_oracle_is_not_clamped() {
        let o = parse(&json!({"status": "FINISHED", "score": {"duration": "REGULAR",
            "fullTime": {"home": 16, "away": 0}}})).unwrap();
        assert_eq!(o.regulation, None);
    }
}
