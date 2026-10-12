//! API-Football (api-sports.io), v3: `GET /fixtures?id={id}` with the `x-apisports-key` header.
//!
//! Field names follow the provider's public documentation; they have not yet been exercised
//! against the live API (that needs a key), only against the captured-shape samples in the tests
//! below. This provider is the better source of the 90-minute score: `score.fulltime` is the
//! score after normal time even when the match went on.

use async_trait::async_trait;
use serde_json::Value;

use super::{ScoreSource, SourceError, goals};
use crate::model::{Observation, Phase};

pub struct ApiFootball {
    client: reqwest::Client,
    base_url: String,
    key: String,
}

impl ApiFootball {
    pub fn new(key: String) -> Self {
        Self::with_base_url("https://v3.football.api-sports.io".into(), key)
    }

    pub fn with_base_url(base_url: String, key: String) -> Self {
        Self { client: reqwest::Client::new(), base_url, key }
    }
}

#[async_trait]
impl ScoreSource for ApiFootball {
    fn id(&self) -> &'static str {
        "api-football"
    }

    async fn observe(&self, external_id: &str) -> Result<Observation, SourceError> {
        let url = format!("{}/fixtures", self.base_url.trim_end_matches('/'));
        let body: Value = self
            .client
            .get(url)
            .query(&[("id", external_id)])
            .header("x-apisports-key", &self.key)
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

pub fn parse(body: &Value) -> Result<Observation, SourceError> {
    let item = body
        .get("response")
        .and_then(|r| r.get(0))
        .ok_or(SourceError::Parse("empty response"))?;
    let short = item
        .pointer("/fixture/status/short")
        .and_then(Value::as_str)
        .ok_or(SourceError::Parse("no status"))?;

    let (phase, went_to_extra_time, went_to_penalties_status) = match short {
        "TBD" | "NS" => (Phase::Scheduled, false, false),
        "1H" => (Phase::FirstHalf, false, false),
        "HT" => (Phase::HalfTime, false, false),
        "2H" => (Phase::SecondHalf, false, false),
        // "ET" doesn't say which half; the later one never holds a result back.
        "ET" => (Phase::ExtraTimeSecondHalf, true, false),
        "BT" => (Phase::ExtraTimeBreak, true, false),
        "P" => (Phase::Penalties, true, true),
        "FT" => (Phase::Finished, false, false),
        "AET" => (Phase::Finished, true, false),
        "PEN" => (Phase::Finished, true, true),
        other => return Err(SourceError::Abnormal(other.to_string())),
    };

    let penalties = goals(item, &["score", "penalty", "home"], &["score", "penalty", "away"]);
    let went_to_penalties = went_to_penalties_status || penalties.is_some();
    let went_to_extra_time = went_to_extra_time || went_to_penalties;

    // `goals` is the score now, including extra time and excluding the shoot-out.
    let score = goals(item, &["goals", "home"], &["goals", "away"]).unwrap_or((0, 0));
    // `score.fulltime` is the 90-minute score. During normal time it's still null, and a match that
    // finished in 90 minutes has the same number in `goals`.
    let regulation = goals(item, &["score", "fulltime", "home"], &["score", "fulltime", "away"])
        .or_else(|| (phase == Phase::Finished && !went_to_extra_time).then_some(score));

    Ok(Observation {
        source: "api-football",
        phase,
        score,
        regulation,
        penalties,
        went_to_extra_time,
        went_to_penalties,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(short: &str, goals: Value, score: Value) -> Value {
        json!({"response": [{"fixture": {"status": {"short": short}}, "goals": goals, "score": score}]})
    }

    fn nothing() -> Value {
        json!({"halftime": {"home": null, "away": null}, "fulltime": {"home": null, "away": null},
               "extratime": {"home": null, "away": null}, "penalty": {"home": null, "away": null}})
    }

    #[test]
    fn live_phases() {
        for (short, phase) in [("NS", Phase::Scheduled), ("1H", Phase::FirstHalf), ("HT", Phase::HalfTime), ("2H", Phase::SecondHalf)] {
            let o = parse(&fixture(short, json!({"home": 1, "away": 0}), nothing())).unwrap();
            assert_eq!(o.phase, phase, "{short}");
            assert_eq!(o.regulation, None, "no 90-minute score yet while the match is live");
        }
    }

    #[test]
    fn a_ninety_minute_finish() {
        let o = parse(&fixture("FT", json!({"home": 2, "away": 1}),
            json!({"fulltime": {"home": 2, "away": 1}, "penalty": {"home": null, "away": null}}))).unwrap();
        assert_eq!((o.phase, o.score, o.regulation), (Phase::Finished, (2, 1), Some((2, 1))));
        assert!(!o.went_to_extra_time);
    }

    #[test]
    fn extra_time_keeps_the_ninety_minute_score_apart_from_the_final_one() {
        let o = parse(&fixture("AET", json!({"home": 3, "away": 1}),
            json!({"fulltime": {"home": 1, "away": 1}, "extratime": {"home": 2, "away": 0},
                   "penalty": {"home": null, "away": null}}))).unwrap();
        assert_eq!((o.score, o.regulation), ((3, 1), Some((1, 1))));
        assert!(o.went_to_extra_time && !o.went_to_penalties);
    }

    #[test]
    fn a_shootout_is_kept_out_of_the_goals() {
        let o = parse(&fixture("PEN", json!({"home": 1, "away": 1}),
            json!({"fulltime": {"home": 1, "away": 1}, "extratime": {"home": 0, "away": 0},
                   "penalty": {"home": 4, "away": 3}}))).unwrap();
        assert_eq!((o.score, o.penalties), ((1, 1), Some((4, 3))));
        assert!(o.went_to_penalties && o.went_to_extra_time);
    }

    #[test]
    fn extra_time_phases() {
        assert_eq!(parse(&fixture("BT", json!({"home": 1, "away": 1}), nothing())).unwrap().phase, Phase::ExtraTimeBreak);
        assert_eq!(parse(&fixture("P", json!({"home": 1, "away": 1}), nothing())).unwrap().phase, Phase::Penalties);
    }

    #[test]
    fn matches_that_are_not_playing_normally_are_refused() {
        for short in ["PST", "CANC", "ABD", "SUSP", "INT", "AWD", "WO"] {
            let r = parse(&fixture(short, json!({"home": null, "away": null}), nothing()));
            assert!(matches!(r, Err(SourceError::Abnormal(_))), "{short}");
        }
    }

    #[test]
    fn an_empty_response_is_an_error_not_a_scheduled_match() {
        assert!(matches!(parse(&json!({"response": []})), Err(SourceError::Parse(_))));
    }
}
