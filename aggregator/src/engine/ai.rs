//! The model-assisted step: what to do with markets the rules left alone.
//!
//! The rules decide almost everything, deterministically. A market that resembles other markets
//! but could not be placed (a nickname, an abbreviation, a paraphrase) is shown to a model along
//! with the clusters it resembles, and the model says which to join, or which other markets it
//! belongs with, and may write a headline. Nothing it says is trusted blindly: `cluster` refuses a
//! hint that breaks a hard conflict (a different season, year or subject), and an answer that names
//! a market or cluster we did not send is dropped.
//!
//! This module is the pure half: the prompt, the parsing and validation of the answer, and the
//! cache key. The network call lives in `services::llm`.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use super::cluster::Hint;
use crate::models::venue::VenueMarket;

/// One lone market as the model sees it.
#[derive(Debug, Clone)]
pub struct AiMarket {
    /// `venue:market_id`.
    pub id: String,
    pub title: String,
    pub event_title: Option<String>,
    pub category: String,
    /// `YYYY-MM-DD`, when it closes.
    pub closes: Option<String>,
    /// Keys of the clusters it resembles.
    pub candidates: Vec<String>,
}

/// One existing cluster as the model sees it.
#[derive(Debug, Clone)]
pub struct AiCluster {
    pub key: String,
    pub title: String,
    pub samples: Vec<String>,
}

pub const SYSTEM_PROMPT: &str = "You group prediction-market listings into events. An event is ONE real-world subject at one \
time: a match, one season of a TV show, a product launch, a token sale, an election, one asset's price on one day. Many \
different questions about that subject belong to the same event. Different seasons, years, matches or assets are DIFFERENT \
events even when the wording is similar. Reply with JSON only.";

pub fn build_prompt(markets: &[AiMarket], clusters: &[AiCluster]) -> String {
    let mut out = String::new();
    out.push_str("Existing events (key, title, example markets):\n");
    for c in clusters {
        out.push_str(&format!("- key={} | {}\n", c.key, c.title));
        for s in &c.samples {
            out.push_str(&format!("    · {s}\n"));
        }
    }
    out.push_str("\nMarkets to place. For each, choose ONE of:\n");
    out.push_str("  - join an existing event: {\"id\":..., \"join\":\"<event key>\"}\n");
    out.push_str("  - group with other markets listed below that are about the same subject: {\"id\":..., \"group\":\"g1\", \"title\":\"<short headline>\"} (same group label for markets that belong together)\n");
    out.push_str("  - leave alone: {\"id\":..., \"join\":null, \"group\":null}\n");
    out.push_str("A headline names the subject (\"Big Brother Naija Season 11\"), never a question.\n\n");
    for m in markets {
        out.push_str(&format!("id={} | {}", m.id, m.title));
        if let Some(event) = &m.event_title {
            out.push_str(&format!(" | event: {event}"));
        }
        out.push_str(&format!(" | category: {}", m.category));
        if let Some(closes) = &m.closes {
            out.push_str(&format!(" | closes: {closes}"));
        }
        if !m.candidates.is_empty() {
            out.push_str(&format!(" | resembles: {}", m.candidates.join(", ")));
        }
        out.push('\n');
    }
    out.push_str("\nAnswer exactly: {\"assignments\":[{\"id\":...,\"join\":...,\"group\":...,\"title\":...}, ...]}");
    out
}

#[derive(Debug, Deserialize)]
struct Answer {
    #[serde(default)]
    assignments: Vec<Assignment>,
}

#[derive(Debug, Deserialize)]
struct Assignment {
    id: String,
    #[serde(default)]
    join: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

/// Reads a model's reply into hints, keeping only what refers to markets and clusters we sent.
/// Tolerates prose around the JSON. `batch` namespaces group labels, so two batches' "g1" are not
/// the same group.
pub fn parse_response(reply: &str, batch: &str, valid_markets: &HashSet<String>, valid_clusters: &HashSet<String>) -> HashMap<String, Hint> {
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else { return HashMap::new() };
    let Ok(answer) = serde_json::from_str::<Answer>(&reply[start..=end]) else { return HashMap::new() };
    let mut hints = HashMap::new();
    for a in answer.assignments {
        if !valid_markets.contains(&a.id) {
            continue;
        }
        let join = a.join.filter(|k| valid_clusters.contains(k));
        let group = a.group.map(|g| g.trim().to_string()).filter(|g| !g.is_empty() && g.len() <= 40).map(|g| format!("{batch}-{g}"));
        let title = a
            .title
            .map(|t| t.trim().trim_end_matches('?').trim().to_string())
            .filter(|t| t.split_whitespace().count() >= 2 && t.len() <= 80);
        // A join and a group are alternatives; the join wins.
        let hint = if join.is_some() { Hint { join, group: None, title: None } } else { Hint { join: None, group, title } };
        hints.insert(a.id, hint);
    }
    hints
}

/// A decision kept between refreshes, with the hash of the text it was made from.
#[derive(Debug, Clone, PartialEq)]
pub struct Cached {
    pub content_hash: String,
    pub join: Option<String>,
    pub group: Option<String>,
    pub title: Option<String>,
}

impl Cached {
    pub fn from_hint(content_hash: String, hint: &Hint) -> Self {
        Self { content_hash, join: hint.join.clone(), group: hint.group.clone(), title: hint.title.clone() }
    }
    pub fn to_hint(&self) -> Hint {
        Hint { join: self.join.clone(), group: self.group.clone(), title: self.title.clone() }
    }
}

/// What a market's classification depends on. A cached answer is reused only while this is
/// unchanged, so an edited title is asked again.
pub fn content_hash(market: &VenueMarket) -> String {
    let event = market.event.as_ref().map_or("", |e| e.title.as_str());
    super::graph::stable_id("h", &format!("{}|{}|{}|{}", market.title, event, market.category.as_str(), market.close_time / 86_400))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sets(markets: &[&str], clusters: &[&str]) -> (HashSet<String>, HashSet<String>) {
        (markets.iter().map(|s| s.to_string()).collect(), clusters.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn a_join_to_a_cluster_we_sent_is_kept() {
        let (m, c) = sets(&["panta:a"], &["ve:jupiter:E1"]);
        let hints = parse_response(r#"{"assignments":[{"id":"panta:a","join":"ve:jupiter:E1"}]}"#, "b1", &m, &c);
        assert_eq!(hints["panta:a"].join.as_deref(), Some("ve:jupiter:E1"));
    }

    #[test]
    fn an_invented_cluster_or_market_is_dropped() {
        let (m, c) = sets(&["panta:a"], &["real"]);
        let hints = parse_response(r#"{"assignments":[{"id":"panta:a","join":"invented"},{"id":"panta:zzz","join":"real"}]}"#, "b1", &m, &c);
        assert!(!hints.contains_key("panta:zzz"));
        assert!(hints["panta:a"].join.is_none(), "an invented cluster is not joined");
    }

    #[test]
    fn groups_are_namespaced_by_batch_and_titles_are_cleaned() {
        let (m, c) = sets(&["a", "b"], &[]);
        let reply = r#"Sure! {"assignments":[{"id":"a","group":"g1","title":"Big Brother Naija Season 11?"},{"id":"b","group":"g1","title":"x"}]} Hope that helps."#;
        let hints = parse_response(reply, "b7", &m, &c);
        assert_eq!(hints["a"].group.as_deref(), Some("b7-g1"));
        assert_eq!(hints["b"].group.as_deref(), Some("b7-g1"));
        assert_eq!(hints["a"].title.as_deref(), Some("Big Brother Naija Season 11"));
        assert!(hints["b"].title.is_none(), "a one-word title is not a headline");
    }

    #[test]
    fn nonsense_gives_no_hints_and_never_panics() {
        let (m, c) = sets(&["a"], &[]);
        for reply in ["", "no json here", "{", "{\"assignments\": 3}", "[1,2,3]"] {
            assert!(parse_response(reply, "b", &m, &c).is_empty(), "{reply:?}");
        }
    }

    #[test]
    fn the_prompt_carries_every_market_and_its_candidates() {
        let prompt = build_prompt(
            &[AiMarket { id: "panta:a".into(), title: "Will Tram finish Top 3?".into(), event_title: None, category: "culture".into(), closes: Some("2026-10-04".into()), candidates: vec!["ve:x:1".into()] }],
            &[AiCluster { key: "ve:x:1".into(), title: "Big Brother Naija".into(), samples: vec!["Who wins?".into()] }],
        );
        for needle in ["panta:a", "Will Tram finish Top 3?", "ve:x:1", "Big Brother Naija", "2026-10-04"] {
            assert!(prompt.contains(needle), "{needle}");
        }
    }

    #[test]
    fn the_cache_key_changes_when_the_text_does() {
        let mut m = VenueMarket { title: "A".into(), ..Default::default() };
        let one = content_hash(&m);
        assert_eq!(one, content_hash(&m));
        m.title = "B".into();
        assert_ne!(one, content_hash(&m));
    }
}
