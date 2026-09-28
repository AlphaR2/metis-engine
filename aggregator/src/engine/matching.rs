//! Cross-venue matching, without AI: canonical keys first, scored candidates second, and a
//! pair is merged only when it is unambiguous. A missed merge costs little; a wrong one ranks two
//! different bets as interchangeable, so anything uncertain stays separate.
//!
//! 1. **Exact keys.** A question whose extraction is complete gets a canonical key - e.g.
//!    `px:bitcoin:above:90000:2026-12-31:touch` or `win:atlanta-falcons|green-bay-packers:2026-09-25`.
//!    Equal keys across venues merge.
//! 2. **Scored candidates.** Anything else is compared only within a block (same domain, target
//!    dates within a day or unknown, at least one shared entity), then:
//!    - hard constraints - different thresholds, directions, dates or subjects mean *different*;
//!    - a weighted score - entity overlap, term similarity, date and number agreement.
//!
//!    A pair merges at `AUTO_MERGE` or above. Every comparison is recorded as a `MatchDecision`.
//! 3. **Overrides** - an operator's merge/split, keyed by venue market ids so they survive rebuilds.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::extract::{Direction, Domain, Extraction, LabelShape};

/// Score at or above which a candidate pair merges automatically.
pub const AUTO_MERGE: f64 = 0.90;

/// Near-misses at or above this are recorded for review (`GET /admin/matches`).
pub const RECORD_FROM: f64 = 0.50;

/// Component weights of the candidate score. Sum to 1.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct ScoreWeights {
    pub entities: f64,
    pub terms: f64,
    pub dates: f64,
    pub numbers: f64,
    pub outcome: f64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self { entities: 0.35, terms: 0.25, dates: 0.15, numbers: 0.15, outcome: 0.10 }
    }
}

/// How a price question settles: at any point before the deadline ("hit", "reach", "at any
/// point"), or at a specific moment ("priced at … on September 8"). Different bets - never merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettleStyle {
    Touch,
    Close,
}

/// What matching needs to know about one question, for one venue.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// `venue:market_id` of every market in the question, sorted - the stable identity used for
    /// overrides.
    pub listing_ids: Vec<String>,
    pub venue_id: String,
    pub extraction: Extraction,
    pub style: Option<SettleStyle>,
    /// For an outcome of a multi-option question: that option's key (slug of its label).
    pub option: Option<String>,
    /// True for a "who wins" question over a matchup.
    pub winner: bool,
}

/// One compared pair and what was decided.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MatchDecision {
    pub left: Vec<String>,
    pub right: Vec<String>,
    pub score: f64,
    pub merged: bool,
    /// `exact_key`, `score`, `override_merge`, `override_split`, or the failed constraint.
    pub reason: String,
    pub features: MatchFeatures,
}

#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct MatchFeatures {
    pub entities: f64,
    pub terms: f64,
    pub dates: f64,
    pub numbers: f64,
    pub outcome: f64,
}

/// Operator decisions that every rebuild honours, keyed by pairs of `venue:market_id`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct MatchOverrides {
    pub merge: Vec<(String, String)>,
    pub split: Vec<(String, String)>,
}

impl MatchOverrides {
    fn find(&self, pairs: &[(String, String)], left: &[String], right: &[String]) -> bool {
        pairs.iter().any(|(a, b)| {
            (left.contains(a) && right.contains(b)) || (left.contains(b) && right.contains(a))
        })
    }

    pub fn forces_merge(&self, left: &[String], right: &[String]) -> bool {
        self.find(&self.merge, left, right)
    }

    pub fn forbids_merge(&self, left: &[String], right: &[String]) -> bool {
        self.find(&self.split, left, right)
    }
}

pub fn settle_style(text: &str, shape: &LabelShape) -> Option<SettleStyle> {
    let folded = super::text::fold(text);
    let touch = ["hit", "reach", "any point", "at any time", "touch", "dip to", "fall to"].iter().any(|w| folded.contains(w))
        || matches!(shape, LabelShape::Threshold { .. });
    let close = ["close above", "close below", "closes above", "closes below", "priced at", "be above", "be below", "settle"]
        .iter()
        .any(|w| folded.contains(w));
    match (touch, close) {
        (true, false) => Some(SettleStyle::Touch),
        (false, true) => Some(SettleStyle::Close),
        _ => None,
    }
}

/// The exact-join key for a question, when its extraction pins it down completely.
pub fn canonical_key(candidate: &Candidate) -> Option<String> {
    let x = &candidate.extraction;
    let date = x.target_date?;
    match x.domain {
        Domain::Crypto | Domain::Finance | Domain::Commodity if candidate.option.is_none() => {
            let subject = x.subject.as_ref()?;
            let direction = match x.direction? {
                Direction::Above => "above",
                Direction::Below => "below",
            };
            let threshold = x.threshold?.normalize();
            let style = match candidate.style? {
                SettleStyle::Touch => "touch",
                SettleStyle::Close => "close",
            };
            Some(format!("px:{}:{direction}:{threshold}:{date}:{style}", subject.key))
        }
        Domain::Sports if x.participants.len() == 2 && candidate.winner => {
            let sides: Vec<&str> = x.participants.iter().map(|p| p.key.as_str()).collect();
            Some(format!("win:{}:{date}", sides.join("|")))
        }
        _ => None,
    }
}

fn jaccard<T: Ord>(a: &BTreeSet<T>, b: &BTreeSet<T>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f64 / a.union(b).count() as f64
}

/// Compares two questions from different venues. `None` when they aren't even in the same block
/// (not worth recording); otherwise the decision, merged or not.
pub fn compare(left: &Candidate, right: &Candidate, weights: &ScoreWeights, overrides: &MatchOverrides) -> Option<MatchDecision> {
    let decision = |score: f64, merged: bool, reason: &str, features: MatchFeatures| MatchDecision {
        left: left.listing_ids.clone(),
        right: right.listing_ids.clone(),
        score,
        merged,
        reason: reason.to_string(),
        features,
    };
    if overrides.forbids_merge(&left.listing_ids, &right.listing_ids) {
        return Some(decision(0.0, false, "override_split", MatchFeatures::default()));
    }
    if overrides.forces_merge(&left.listing_ids, &right.listing_ids) {
        return Some(decision(1.0, true, "override_merge", MatchFeatures::default()));
    }
    if left.venue_id == right.venue_id {
        return None;
    }
    if let (Some(a), Some(b)) = (canonical_key(left), canonical_key(right))
        && a == b
    {
        return Some(decision(1.0, true, "exact_key", MatchFeatures { entities: 1.0, terms: 1.0, dates: 1.0, numbers: 1.0, outcome: 1.0 }));
    }

    let (x, y) = (&left.extraction, &right.extraction);
    // ── Blocking ────────────────────────────────────────────────────────────────────────────
    let entities_x: BTreeSet<&str> = x.entities.iter().map(|e| e.key.as_str()).collect();
    let entities_y: BTreeSet<&str> = y.entities.iter().map(|e| e.key.as_str()).collect();
    let shared_entities = entities_x.intersection(&entities_y).count();
    if x.domain != y.domain || shared_entities == 0 {
        return None;
    }
    let date_gap = match (x.target_date, y.target_date) {
        (Some(a), Some(b)) => Some((a - b).num_days().abs()),
        _ => None,
    };
    if date_gap.is_some_and(|gap| gap > 1) {
        return None;
    }

    // ── Hard constraints ────────────────────────────────────────────────────────────────────
    let constraint = |reason: &str| Some(decision(0.0, false, reason, MatchFeatures::default()));
    if let (Some(a), Some(b)) = (&x.subject, &y.subject)
        && a.key != b.key
    {
        return constraint("different_subject");
    }
    if let (Some(a), Some(b)) = (x.threshold, y.threshold)
        && a.normalize() != b.normalize()
    {
        return constraint("different_threshold");
    }
    if let (Some(a), Some(b)) = (x.direction, y.direction)
        && a != b
    {
        return constraint("opposite_direction");
    }
    if let (Some(a), Some(b)) = (left.style, right.style)
        && a != b
    {
        return constraint("different_settlement");
    }
    if x.participants.len() == 2 && y.participants.len() == 2 && x.participants != y.participants {
        return constraint("different_participants");
    }
    // An option of a multi-choice question can only be one side of a binary about that option.
    if let (Some(a), Some(b)) = (&left.option, &right.option)
        && a != b
    {
        return constraint("different_option");
    }
    if let Some(option) = left.option.as_ref().or(right.option.as_ref()) {
        let other = if left.option.is_some() { y } else { x };
        if !other.entities.iter().any(|e| &e.key == option) {
            return constraint("option_not_named");
        }
    }

    // ── Score ───────────────────────────────────────────────────────────────────────────────
    let features = MatchFeatures {
        entities: jaccard(&entities_x, &entities_y),
        terms: jaccard(&x.terms, &y.terms),
        dates: match date_gap {
            Some(0) => 1.0,
            Some(1) => 0.5,
            _ => 0.0,
        },
        numbers: match (x.threshold, y.threshold) {
            (Some(_), Some(_)) => 1.0,
            (None, None) => 0.5,
            _ => 0.0,
        },
        outcome: match (&left.option, &right.option) {
            (Some(a), Some(b)) if a == b => 1.0,
            (Some(_), None) | (None, Some(_)) => 1.0,
            _ => 0.5,
        },
    };
    // Terms are compared after removing entity words, which the entity feature already counts,
    // and saturate: a quarter of the words in common is already strong agreement between two
    // venues' very differently phrased titles.
    let terms = (features.terms * 4.0).min(1.0);
    let score = weights.entities * features.entities.min(1.0).max(if shared_entities > 0 { 0.5 } else { 0.0 })
        + weights.terms * terms
        + weights.dates * features.dates
        + weights.numbers * features.numbers
        + weights.outcome * features.outcome;
    let merged = score >= AUTO_MERGE;
    Some(decision(score, merged, if merged { "score" } else { "below_threshold" }, features))
}

/// Groups candidates into merge sets across venues. Returns, for each candidate index, the index
/// of the candidate it merges into (its set's representative), plus every recorded decision.
///
/// A candidate merges with at most one candidate per other venue - the best-scoring one - so one
/// venue's question can never swallow two different questions from another venue.
pub fn resolve(candidates: &[Candidate], weights: &ScoreWeights, overrides: &MatchOverrides) -> (Vec<usize>, Vec<MatchDecision>) {
    let n = candidates.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(parent: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while parent[r] != r {
            r = parent[r];
        }
        let mut j = i;
        while parent[j] != r {
            let next = parent[j];
            parent[j] = r;
            j = next;
        }
        r
    }

    // Venues already present in each merge set, by root: a set never takes two questions from
    // the same venue.
    let mut venues: Vec<HashSet<String>> = candidates.iter().map(|c| HashSet::from([c.venue_id.clone()])).collect();
    let union = |parent: &mut Vec<usize>, venues: &mut Vec<HashSet<String>>, a: usize, b: usize| -> bool {
        let (ra, rb) = (root(parent, a), root(parent, b));
        if ra == rb || !venues[ra].is_disjoint(&venues[rb]) {
            return false;
        }
        let moved = std::mem::take(&mut venues[ra]);
        venues[rb].extend(moved);
        parent[ra] = rb;
        true
    };

    // Exact keys join in one pass.
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut decisions = Vec::new();
    for (i, c) in candidates.iter().enumerate() {
        if let Some(key) = canonical_key(c) {
            match by_key.get(&key) {
                Some(&j)
                    if !overrides.forbids_merge(&candidates[j].listing_ids, &c.listing_ids)
                        && union(&mut parent, &mut venues, i, j) =>
                {
                    decisions.push(MatchDecision {
                        left: candidates[j].listing_ids.clone(),
                        right: c.listing_ids.clone(),
                        score: 1.0,
                        merged: true,
                        reason: "exact_key".to_string(),
                        features: MatchFeatures { entities: 1.0, terms: 1.0, dates: 1.0, numbers: 1.0, outcome: 1.0 },
                    });
                }
                Some(_) => {}
                None => {
                    by_key.insert(key, i);
                }
            }
        }
    }

    // Scored candidates, blocked by shared entity so cost grows with overlap, not n².
    let mut by_entity: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, c) in candidates.iter().enumerate() {
        for e in &c.extraction.entities {
            by_entity.entry(e.key.as_str()).or_default().push(i);
        }
    }
    let mut compared: HashSet<(usize, usize)> = HashSet::new();
    let mut best: HashMap<(usize, String), (f64, usize, MatchDecision)> = HashMap::new();
    for bucket in by_entity.values() {
        for (a_pos, &a) in bucket.iter().enumerate() {
            for &b in &bucket[a_pos + 1..] {
                let pair = (a.min(b), a.max(b));
                if candidates[a].venue_id == candidates[b].venue_id || !compared.insert(pair) {
                    continue;
                }
                if root(&mut parent, a) == root(&mut parent, b) {
                    continue;
                }
                let Some(decision) = compare(&candidates[pair.0], &candidates[pair.1], weights, overrides) else { continue };
                if decision.score >= RECORD_FROM || decision.reason.starts_with("override") {
                    decisions.push(decision.clone());
                }
                if decision.merged {
                    for (from, to) in [(pair.0, pair.1), (pair.1, pair.0)] {
                        let slot = (from, candidates[to].venue_id.clone());
                        let better = best.get(&slot).is_none_or(|(score, _, _)| decision.score > *score);
                        if better {
                            best.insert(slot, (decision.score, to, decision.clone()));
                        }
                    }
                }
            }
        }
    }
    // Keep a merge only when each side is the other's best match on that venue.
    let mut mutual: Vec<(usize, usize)> = Vec::new();
    for ((from, _), (_, to, _)) in &best {
        let back = best.get(&(*to, candidates[*from].venue_id.clone()));
        if back.is_some_and(|(_, target, _)| target == from) && from < to {
            mutual.push((*from, *to));
        }
    }
    mutual.sort();
    for (a, b) in mutual {
        union(&mut parent, &mut venues, a, b);
    }
    let roots = (0..n).map(|i| root(&mut parent, i)).collect();
    (roots, decisions)
}

/// Sorted `venue:market_id` identities for a set of markets.
pub fn listing_ids<'a>(venue_id: &str, market_ids: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut ids: Vec<String> = market_ids.into_iter().map(|m| format!("{venue_id}:{m}")).collect();
    ids.sort();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::dictionary::{Dictionary, EntityKind};
    use crate::engine::extract::extract;
    use crate::models::venue::{VenueEvent, VenueMarket};

    fn candidate(d: &Dictionary, venue: &str, id: &str, title: &str, event_title: Option<&str>, option: Option<&str>) -> Candidate {
        let market = VenueMarket {
            venue_id: venue.into(),
            venue_market_id: id.into(),
            title: title.into(),
            event: event_title.map(|t| VenueEvent { event_id: "e".into(), title: t.into(), ..Default::default() }),
            close_time: 1_789_000_000,
            resolution_time: 1_789_086_400,
            ..Default::default()
        };
        let extraction = extract(&market, d);
        let full = format!("{} {title}", event_title.unwrap_or(""));
        Candidate {
            listing_ids: listing_ids(venue, [id]),
            venue_id: venue.into(),
            style: settle_style(&full, &extraction.label_shape),
            option: option.map(crate::engine::text::slug),
            winner: false,
            extraction,
        }
    }

    fn seeded() -> Dictionary {
        Dictionary::seeded()
    }

    #[test]
    fn identical_price_questions_share_a_key_across_venues() {
        let d = seeded();
        let panta = candidate(&d, "panta", "p1", "Will Bitcoin reach $90,000 by December 31, 2026?", None, None);
        let jupiter = candidate(&d, "jupiter", "j1", "↑ 90,000", Some("What price will Bitcoin hit by December 31, 2026?"), None);
        assert_eq!(canonical_key(&panta), Some("px:bitcoin:above:90000:2026-12-31:touch".to_string()));
        assert_eq!(canonical_key(&panta), canonical_key(&jupiter));
        let (roots, decisions) = resolve(&[panta, jupiter], &ScoreWeights::default(), &MatchOverrides::default());
        assert_eq!(roots[0], roots[1]);
        assert_eq!(decisions[0].reason, "exact_key");
    }

    #[test]
    fn hitting_a_price_and_closing_above_it_are_different_bets() {
        let d = seeded();
        let touch = candidate(&d, "jupiter", "j1", "↑ 90,000", Some("What price will Bitcoin hit by December 31, 2026?"), None);
        let close = candidate(&d, "panta", "p1", "Will Bitcoin be priced at $90,000 or higher on December 31, 2026?", None, None);
        assert_ne!(canonical_key(&touch), canonical_key(&close));
        let decision = compare(&touch, &close, &ScoreWeights::default(), &MatchOverrides::default()).unwrap();
        assert!(!decision.merged);
        assert_eq!(decision.reason, "different_settlement");
    }

    #[test]
    fn a_different_threshold_is_never_merged_however_similar_the_words() {
        let d = seeded();
        let a = candidate(&d, "panta", "p1", "Will Bitcoin be priced at $81,000 or higher on September 8, 2026?", None, None);
        let b = candidate(&d, "jupiter", "j1", "Will Bitcoin be priced at $82,000 or higher on September 8, 2026?", None, None);
        let decision = compare(&a, &b, &ScoreWeights::default(), &MatchOverrides::default()).unwrap();
        assert_eq!(decision.reason, "different_threshold");
        assert!(!decision.merged);
    }

    #[test]
    fn a_binary_about_one_option_matches_that_option_of_a_multi_choice_question() {
        let mut d = seeded();
        for name in ["Marco Rubio", "J.D. Vance"] {
            d.learn_participant(name, None, EntityKind::Person);
        }
        let panta = candidate(&d, "panta", "p1", "Marco Rubio to win the 2028 Presidential nomination of the Republican Party", None, None);
        let rubio = candidate(&d, "jupiter", "j1", "Marco Rubio", Some("Republican Presidential Nominee 2028"), Some("Marco Rubio"));
        let vance = candidate(&d, "jupiter", "j2", "J.D. Vance", Some("Republican Presidential Nominee 2028"), Some("J.D. Vance"));

        let decision = compare(&panta, &rubio, &ScoreWeights::default(), &MatchOverrides::default()).unwrap();
        assert!(decision.merged, "{decision:?}");
        assert!(compare(&panta, &vance, &ScoreWeights::default(), &MatchOverrides::default()).is_none_or(|d| !d.merged));

        let (roots, _) = resolve(&[panta, rubio, vance], &ScoreWeights::default(), &MatchOverrides::default());
        assert_eq!(roots[0], roots[1]);
        assert_ne!(roots[0], roots[2]);
    }

    #[test]
    fn overrides_win_over_the_score_in_both_directions() {
        let d = seeded();
        let a = candidate(&d, "panta", "p1", "Will Bitcoin be priced at $81,000 or higher on September 8, 2026?", None, None);
        let b = candidate(&d, "jupiter", "j1", "Will Bitcoin be priced at $82,000 or higher on September 8, 2026?", None, None);
        let merge = MatchOverrides { merge: vec![("panta:p1".into(), "jupiter:j1".into())], split: vec![] };
        assert!(compare(&a, &b, &ScoreWeights::default(), &merge).unwrap().merged);

        let c = candidate(&d, "jupiter", "j2", "↑ 90,000", Some("What price will Bitcoin hit by December 31, 2026?"), None);
        let e = candidate(&d, "panta", "p2", "Will Bitcoin reach $90,000 by December 31, 2026?", None, None);
        let split = MatchOverrides { merge: vec![], split: vec![("panta:p2".into(), "jupiter:j2".into())] };
        let (roots, _) = resolve(&[c, e], &ScoreWeights::default(), &split);
        assert_ne!(roots[0], roots[1], "a split override beats even an exact key");
    }

    #[test]
    fn one_question_never_absorbs_two_from_the_same_other_venue() {
        let d = seeded();
        let panta = candidate(&d, "panta", "p1", "Will Bitcoin reach $90,000 by December 31, 2026?", None, None);
        let j1 = candidate(&d, "jupiter", "j1", "↑ 90,000", Some("What price will Bitcoin hit by December 31, 2026?"), None);
        let j2 = candidate(&d, "jupiter", "j2", "↑ 90,000", Some("Bitcoin price hit by December 31, 2026?"), None);
        let (roots, _) = resolve(&[panta, j1, j2], &ScoreWeights::default(), &MatchOverrides::default());
        assert_eq!(roots[0], roots[1]);
        assert_ne!(roots[1], roots[2], "a set never holds two questions from one venue");
    }
}
