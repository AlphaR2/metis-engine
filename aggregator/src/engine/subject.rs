//! What a market is *about*: the subject its event is built around.
//!
//! A profile is derived from the market exactly as the venue sent it (title, its venue event's
//! title and tags, outcome labels, dates) plus the entity dictionary. It never changes the market;
//! it only gives `cluster` something to compare. The pieces:
//!
//! - **entities** the text names (assets, teams, countries), and for a matchup its two sides;
//! - **anchors** that pin a subject in time: a season number, a year, a target date;
//! - **terms**: the distinctive words, with a few aliases expanded so "BBNaija" and "Big Brother
//!   Naija" meet.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use regex::Regex;
use rust_decimal::Decimal;
use std::sync::LazyLock;

use super::dictionary::{Dictionary, EntityRef};
use super::extract::{self, Domain};
use super::rating::volume_of;
use super::text;
use crate::models::venue::VenueMarket;

/// A venue market's identity: `venue:market_id`.
pub fn market_key(market: &VenueMarket) -> String {
    format!("{}:{}", market.venue_id, market.venue_market_id)
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub key: String,
    /// The venue's own event grouping, when it has one.
    pub venue_event: Option<(String, String)>,
    pub entities: Vec<EntityRef>,
    /// The two sides of a matchup, sorted by key.
    pub participants: Vec<EntityRef>,
    /// The asset a price question is about.
    pub subject: Option<EntityRef>,
    /// The date the market is about: a match day or a price deadline.
    pub target_date: Option<NaiveDate>,
    pub close_date: Option<NaiveDate>,
    pub domain: Domain,
    pub terms: BTreeSet<String>,
    /// "Season 11", "S11".
    pub season: Option<u32>,
    pub years: Vec<i32>,
    pub close_time: i64,
    pub volume: Decimal,
}

/// Spelled-out expansions of compounds and abbreviations that recur in prediction titles. A small,
/// data-only table: whatever it misses is left to the model-assisted step.
const TERM_ALIASES: &[(&str, &[&str])] = &[
    ("bbnaija", &["big", "brother", "naija"]),
    ("bbn", &["big", "brother", "naija"]),
    ("ucl", &["champions", "league"]),
    ("epl", &["premier", "league"]),
    ("nba", &["basketball", "nba"]),
    ("gta", &["grand", "theft", "auto"]),
    ("man", &["manchester"]),
    ("utd", &["united"]),
    ("spurs", &["tottenham"]),
];

/// Words that open or pad a prediction title without saying what it is about.
const FILLER: &[&str] = &[
    "finish", "top", "reach", "hit", "above", "below", "least", "most", "more", "less", "over", "under", "before", "after",
    "release", "released", "announce", "announced", "exceed", "exceeds", "target", "deadline", "female", "male",
];

static SEASON: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bseason\s*(\d{1,2})\b|\bs(\d{1,2})\b").unwrap());

pub fn season_of(text: &str) -> Option<u32> {
    SEASON.captures(text).and_then(|c| c.get(1).or_else(|| c.get(2))).and_then(|m| m.as_str().parse().ok())
}

/// The distinctive terms of `text`, with aliases expanded and filler dropped.
pub fn subject_terms(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for term in text::terms(text) {
        match TERM_ALIASES.iter().find(|(alias, _)| *alias == term) {
            Some((_, expansion)) => out.extend(expansion.iter().map(|s| s.to_string())),
            None => {
                if !FILLER.contains(&term.as_str()) {
                    out.insert(term);
                }
            }
        }
    }
    out
}

pub fn profile(market: &VenueMarket, dictionary: &Dictionary) -> Profile {
    let extraction = extract::extract(market, dictionary);
    let mut text = String::new();
    text.push_str(&market.title);
    if let Some(event) = &market.event {
        text.push(' ');
        text.push_str(&event.title);
        for tag in &event.tags {
            text.push(' ');
            text.push_str(tag);
        }
    }
    let years = {
        let mut y = text::years(&text);
        y.sort_unstable();
        y.dedup();
        y
    };
    Profile {
        key: market_key(market),
        venue_event: market.event.as_ref().map(|e| (market.venue_id.clone(), e.event_id.clone())),
        entities: extraction.entities,
        participants: extraction.participants,
        subject: extraction.subject,
        target_date: extraction.target_date,
        close_date: text::date_of(market.close_time),
        domain: extraction.domain,
        terms: subject_terms(&text),
        season: season_of(&text),
        years,
        close_time: market.close_time,
        volume: volume_of(market),
    }
}

/// Whether two subjects cannot be the same event, whatever their words share: a different season
/// of a show, a different year, a different kind of subject, or betting that closes far apart.
pub fn conflicts(a: &Profile, b: &Profile) -> bool {
    if let (Some(x), Some(y)) = (a.season, b.season)
        && x != y
    {
        return true;
    }
    if !a.years.is_empty() && !b.years.is_empty() && !a.years.iter().any(|y| b.years.contains(y)) {
        return true;
    }
    if a.domain != Domain::Other && b.domain != Domain::Other && a.domain != b.domain {
        return true;
    }
    close_apart(a.close_time, b.close_time)
}

/// Betting that closes more than this many days apart is not the same moment's event.
pub const MAX_CLOSE_GAP_DAYS: i64 = 90;

pub fn close_apart(a: i64, b: i64) -> bool {
    a > 0 && b > 0 && (a - b).abs() > MAX_CLOSE_GAP_DAYS * 86_400
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn season_numbers_are_read_in_both_spellings() {
        assert_eq!(season_of("Big Brother Naija Season 11"), Some(11));
        assert_eq!(season_of("BBNaija S11 winner"), Some(11));
        assert_eq!(season_of("Will it rain"), None);
    }

    #[test]
    fn bbnaija_and_big_brother_naija_share_their_terms() {
        let a = subject_terms("Will Tram finish in the Top 3 of BBNaija Season 11?");
        let b = subject_terms("A female housemate to win Big Brother Naija Season 11?");
        for word in ["big", "brother", "naija", "season"] {
            assert!(a.contains(word), "{word} missing from {a:?}");
            assert!(b.contains(word), "{word} missing from {b:?}");
        }
        // The market-specific part stays specific.
        assert!(a.contains("tram") && !b.contains("tram"));
    }

    #[test]
    fn a_different_season_or_year_conflicts() {
        let base = |season: Option<u32>, years: Vec<i32>| Profile {
            key: "v:m".into(),
            venue_event: None,
            entities: vec![],
            participants: vec![],
            subject: None,
            target_date: None,
            close_date: None,
            domain: Domain::Other,
            terms: BTreeSet::new(),
            season,
            years,
            close_time: 0,
            volume: Decimal::ZERO,
        };
        assert!(conflicts(&base(Some(11), vec![]), &base(Some(10), vec![])));
        assert!(!conflicts(&base(Some(11), vec![]), &base(Some(11), vec![])));
        assert!(conflicts(&base(None, vec![2026]), &base(None, vec![2027])));
        assert!(!conflicts(&base(None, vec![2026]), &base(None, vec![2026, 2027])));
        assert!(!conflicts(&base(None, vec![]), &base(Some(3), vec![2026])));
    }

    #[test]
    fn betting_that_closes_far_apart_conflicts() {
        assert!(close_apart(1_000_000, 1_000_000 + 91 * 86_400));
        assert!(!close_apart(1_000_000, 1_000_000 + 10 * 86_400));
        assert!(!close_apart(0, 1_000_000), "no close time says nothing");
    }
}
