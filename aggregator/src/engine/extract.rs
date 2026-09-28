//! Deterministic extraction: what a market is about, pulled from its text and the structured
//! fields the venue sent. No model calls - rules, the entity dictionary and number/date parsing.
//!
//! The output feeds two things: question text and structure (`questions`), and matching keys
//! (`matching`). Every field is optional in spirit - an extraction that finds little simply leads
//! to a lower `confidence` and a market that is served as-is and never auto-merged.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use chrono::{Datelike, NaiveDate};
use regex::Regex;
use rust_decimal::Decimal;
use serde::Serialize;

use super::dictionary::{Dictionary, EntityKind, EntityRef};
use super::text::{self, PartialDate};
use crate::models::venue::VenueMarket;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Sports,
    Crypto,
    Finance,
    Commodity,
    Politics,
    Weather,
    Other,
}

impl Domain {
    pub fn as_str(self) -> &'static str {
        match self {
            Domain::Sports => "sports",
            Domain::Crypto => "crypto",
            Domain::Finance => "finance",
            Domain::Commodity => "commodity",
            Domain::Politics => "politics",
            Domain::Weather => "weather",
            Domain::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Above,
    Below,
}

/// What a market's own label looks like, when it sits under an event that holds the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelShape {
    /// "↑ 90,000", "Above $5,000".
    Threshold { direction: Direction, value: Decimal },
    /// "160-179", "$2.0–$2.25T".
    Range { low: Decimal, high: Decimal },
    /// "4", "15+".
    Count(Decimal),
    /// "December 31".
    Date(PartialDate),
    /// "Yes", "No", "Up", "Down".
    YesNo,
    /// A name: a candidate, a team, an option.
    Name,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Extraction {
    pub domain: Domain,
    /// Every known entity mentioned or attached.
    pub entities: Vec<EntityRef>,
    /// The two sides of a matchup ("A vs B"), sorted by key.
    pub participants: Vec<EntityRef>,
    /// The main asset for a price question.
    pub subject: Option<EntityRef>,
    pub direction: Option<Direction>,
    /// The price or level a threshold question is about.
    pub threshold: Option<Decimal>,
    /// The date the question is about - the match day, the price deadline.
    pub target_date: Option<NaiveDate>,
    pub label_shape: LabelShape,
    /// Meaningful words of the full text, for similarity.
    pub terms: BTreeSet<String>,
    /// 0-1: how much of what a question needs was found.
    pub confidence: f32,
}

static MATCHUP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:[^:]*:\s*)?(.+?)\s+(?:vs\.?|v\.?|versus|@)\s+(.+?)(?:\s*(?:\(|\s-\s|\||,|\?|:)|$)").unwrap()
});
static BEAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bwill\s+(?:the\s+)?(.+?)\s+(?:beat|defeat|win against|overcome)\s+(?:the\s+)?(.+?)(?:\s+(?:by|in|and|while|to|on|at)\b|\?|,|$)").unwrap()
});
static ABOVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(↑|\babove\b|\bor higher\b|\bor above\b|\bover\b|\bexceed|\breach(?:es)?\b|\bhit(?:s)?\b|\bmore than\b|\bat least\b|\bgreater than\b|\bhigher than\b)").unwrap()
});
static BELOW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(↓|\bbelow\b|\bunder\b|\bor lower\b|\bor below\b|\bless than\b|\bfall(?:s)? to\b|\bdip(?:s)? to\b|\bdrop(?:s)? to\b|\blower than\b)").unwrap()
});
/// A unit a bucket label may carry: magnitude, percent, temperature.
const UNIT: &str = r"(?:[kKmMbBtT%]|°\s*[CF])?";
static RANGE_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"^\s*[<>≤≥]?\s*\$?[\d.,]+\s*{UNIT}\s*(?:-|–|to)\s*\$?[\d.,]+\s*{UNIT}\s*$")).unwrap()
});
/// A single bucket: "4", "15+", "25°C".
static COUNT_LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^\s*[\d.,]+\s*\+?\s*{UNIT}\s*$")).unwrap());

static POLITICS_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(elections?|nominee|nomination|president|presidential|prime minister|senate|governor|parliament|vote|by-election|ceasefire|invade|regime)\b").unwrap()
});
static WEATHER_WORDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\brain\b|\btemperature\b|\bsnow\b|\bhurricane\b|\bweather\b|°)").unwrap());
const SPORTS_CATEGORIES: &[&str] = &["sports", "esports"];

pub fn classify_label(label: &str) -> LabelShape {
    let trimmed = label.trim();
    let folded = text::fold(trimmed);
    if matches!(folded.as_str(), "yes" | "no" | "up" | "down") {
        return LabelShape::YesNo;
    }
    if RANGE_LABEL.is_match(trimmed) {
        let n = text::numbers(trimmed);
        if n.len() >= 2 {
            return LabelShape::Range { low: n[0], high: n[1] };
        }
    }
    if COUNT_LABEL.is_match(trimmed)
        && let Some(n) = text::numbers(trimmed).first()
    {
        return LabelShape::Count(*n);
    }
    let numbers = text::numbers(trimmed);
    if numbers.len() == 1 {
        let direction = if BELOW.is_match(trimmed) {
            Some(Direction::Below)
        } else if ABOVE.is_match(trimmed) || trimmed.starts_with(['↑', '>']) {
            Some(Direction::Above)
        } else {
            None
        };
        if let Some(direction) = direction {
            return LabelShape::Threshold { direction, value: numbers[0] };
        }
    }
    if let [date] = text::dates(trimmed).as_slice()
        && trimmed.len() <= 20
    {
        return LabelShape::Date(*date);
    }
    LabelShape::Name
}

/// Extracts what `market` is about. `dictionary` should already hold participants learned from
/// the whole catalog (see `learn_from`), so a team named on one venue is recognised on another.
pub fn extract(market: &VenueMarket, dictionary: &Dictionary) -> Extraction {
    let event_title = market.event.as_ref().map(|e| e.title.as_str()).unwrap_or("");
    let label = market.title.as_str();
    let has_event_question = !event_title.is_empty() && !text::fold(event_title).eq(&text::fold(label));
    let label_shape = if has_event_question { classify_label(label) } else { LabelShape::YesNo };
    let full_text = if has_event_question { format!("{event_title} {label}") } else { label.to_string() };

    // Entities: the venue's own attachment first, then everything the text names.
    let mut entities: Vec<EntityRef> = Vec::new();
    if let Some(entity) = &market.entity
        && let Some(known) = dictionary.lookup(&entity.name)
    {
        entities.push(known);
    }
    for found in dictionary.find_all(&full_text) {
        if !entities.contains(&found) {
            entities.push(found);
        }
    }
    for outcome in &market.outcome_labels {
        if let Some(known) = dictionary.lookup(outcome)
            && !entities.contains(&known)
        {
            entities.push(known);
        }
    }

    let participants = participants(event_title, label, &market.outcome_labels, dictionary);
    let subject = entities
        .iter()
        .find(|e| matches!(e.kind, EntityKind::Asset | EntityKind::Stock | EntityKind::Index | EntityKind::Commodity))
        .cloned();

    let (direction, threshold) = match &label_shape {
        LabelShape::Threshold { direction, value } => (Some(*direction), Some(*value)),
        _ if subject.is_some() => {
            let question_text = if has_event_question && !matches!(label_shape, LabelShape::Name) { &full_text } else { label };
            let direction = if BELOW.is_match(question_text) {
                Some(Direction::Below)
            } else if ABOVE.is_match(question_text) {
                Some(Direction::Above)
            } else {
                None
            };
            // The price is the largest non-year number: "$81,000.00 on September 8, 2026".
            let threshold = text::numbers(question_text)
                .into_iter()
                .filter(|n| !(Decimal::from(1900)..=Decimal::from(2100)).contains(n) || n.fract() != Decimal::ZERO)
                .filter(|n| *n > Decimal::ZERO)
                .max();
            (direction, threshold)
        }
        _ => (None, None),
    };

    let reference = text::date_of(market.close_time).or_else(|| text::date_of(market.resolution_time));
    let target_date = target_date(&full_text, &label_shape, market, reference);

    let domain = domain_of(market, &entities, &participants, &full_text);
    let terms = text::terms(&full_text);

    let wanted = match domain {
        Domain::Sports => 3.0,
        Domain::Crypto | Domain::Finance | Domain::Commodity => 4.0,
        _ => 2.0,
    };
    let found = match domain {
        Domain::Sports => {
            (participants.len() == 2) as u8 as f32 * 2.0 + target_date.is_some() as u8 as f32
        }
        Domain::Crypto | Domain::Finance | Domain::Commodity => {
            subject.is_some() as u8 as f32
                + direction.is_some() as u8 as f32
                + threshold.is_some() as u8 as f32
                + target_date.is_some() as u8 as f32
        }
        _ => (!entities.is_empty()) as u8 as f32 + target_date.is_some() as u8 as f32,
    };

    Extraction {
        domain,
        entities,
        participants,
        subject,
        direction,
        threshold,
        target_date,
        label_shape,
        terms,
        confidence: (found / wanted).min(1.0),
    }
}

/// The two sides of a matchup: from the venue's outcome labels when it lists exactly two named
/// outcomes, else from "A vs B" in the event title, else from "Will A beat B" in the question.
fn participants(event_title: &str, label: &str, outcome_labels: &[String], dictionary: &Dictionary) -> Vec<EntityRef> {
    let resolve = |name: &str| -> EntityRef {
        let name = name.trim().trim_end_matches(['?', '.']);
        dictionary.lookup(name).unwrap_or_else(|| {
            let found = dictionary.find_all(name);
            if found.len() == 1 {
                found[0].clone()
            } else {
                EntityRef { key: text::slug(name), name: name.to_string(), kind: EntityKind::Team }
            }
        })
    };
    let named: Vec<&String> =
        outcome_labels.iter().filter(|o| !matches!(text::fold(o).as_str(), "yes" | "no" | "over" | "under")).collect();
    let mut sides: Vec<EntityRef> = if named.len() == 2 {
        named.iter().map(|o| resolve(o)).collect()
    } else if let Some(caps) = MATCHUP.captures(event_title) {
        vec![resolve(&caps[1]), resolve(&caps[2])]
    } else if let Some(caps) = BEAT.captures(label) {
        vec![resolve(&caps[1]), resolve(&caps[2])]
    } else if let Some(caps) = MATCHUP.captures(label) {
        vec![resolve(&caps[1]), resolve(&caps[2])]
    } else {
        vec![]
    };
    sides.retain(|s| !s.key.is_empty());
    sides.sort();
    sides.dedup();
    if sides.len() == 2 { sides } else { vec![] }
}

fn target_date(full_text: &str, shape: &LabelShape, market: &VenueMarket, reference: Option<NaiveDate>) -> Option<NaiveDate> {
    let reference = reference?;
    if let LabelShape::Date(date) = shape {
        return date.resolve(reference);
    }
    if let Some(date) = text::dates(full_text).first() {
        return date.resolve(reference);
    }
    // "in 2026", "by end of 2026", "before 2027": the year's last day (or, for "before", the one
    // before it).
    let folded = text::fold(full_text);
    if let Some(year) = text::years(full_text).into_iter().max() {
        let year = if folded.contains(&format!("before {year}")) { year - 1 } else { year };
        return NaiveDate::from_ymd_opt(year, 12, 31);
    }
    // A matchup or a same-day question: the day it happens is when betting closes.
    let kickoff = market.event.as_ref().and_then(|e| e.start_time).and_then(text::date_of);
    kickoff.or(Some(reference)).filter(|d| d.year() > 2000)
}

fn domain_of(market: &VenueMarket, entities: &[EntityRef], participants: &[EntityRef], full_text: &str) -> Domain {
    let folded = text::fold(full_text);
    let kinds: Vec<EntityKind> = entities.iter().map(|e| e.kind).collect();
    if kinds.contains(&EntityKind::Asset) {
        return Domain::Crypto;
    }
    if kinds.contains(&EntityKind::Stock) || kinds.contains(&EntityKind::Index) {
        return Domain::Finance;
    }
    if kinds.contains(&EntityKind::Commodity) {
        return Domain::Commodity;
    }
    // Word-bounded: "Ukraine" must not read as "rain".
    if WEATHER_WORDS.is_match(&folded) {
        return Domain::Weather;
    }
    // Structure the venue attached (a matchup, a team, a market kind) is reliable; its category
    // is not - Panta files nomination and price markets under `sports` - so the category is only
    // the last resort.
    if participants.len() == 2 || market.entity.is_some() || market.market_kind.is_some() {
        return Domain::Sports;
    }
    if POLITICS_WORDS.is_match(&folded) {
        return Domain::Politics;
    }
    if SPORTS_CATEGORIES.contains(&market.category.as_str()) {
        return Domain::Sports;
    }
    Domain::Other
}

/// Teaches the dictionary every participant the venues attach to markets (Jupiter's `team`), so
/// extraction recognises them in any venue's text.
pub fn learn_from<'a>(markets: impl IntoIterator<Item = &'a VenueMarket>, dictionary: &mut Dictionary) {
    for market in markets {
        if let Some(entity) = &market.entity {
            dictionary.learn_participant(&entity.name, entity.abbreviation.as_deref(), EntityKind::Team);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::venue::{VenueEntity, VenueEvent};

    fn market(title: &str, event_title: Option<&str>) -> VenueMarket {
        VenueMarket {
            venue_id: "v".into(),
            venue_market_id: "m".into(),
            title: title.into(),
            event: event_title.map(|t| VenueEvent { event_id: "e".into(), title: t.into(), ..Default::default() }),
            close_time: 1_790_000_000,
            resolution_time: 1_790_086_400,
            ..Default::default()
        }
    }

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn a_panta_style_price_question_is_fully_extracted() {
        let x = extract(
            &market("Will Bitcoin (BTC) be priced at $81,000.00 or higher on Tuesday, September 8, 2026, at 12:00 PM ET?", None),
            &Dictionary::seeded(),
        );
        assert_eq!(x.domain, Domain::Crypto);
        assert_eq!(x.subject.unwrap().key, "bitcoin");
        assert_eq!(x.direction, Some(Direction::Above));
        assert_eq!(x.threshold, Some(d("81000")));
        assert_eq!(x.target_date, NaiveDate::from_ymd_opt(2026, 9, 8));
        assert_eq!(x.confidence, 1.0);
    }

    #[test]
    fn a_jupiter_style_threshold_label_under_an_event_is_extracted() {
        let x = extract(&market("↑ 90,000", Some("What price will Bitcoin hit in 2026?")), &Dictionary::seeded());
        assert_eq!(x.label_shape, LabelShape::Threshold { direction: Direction::Above, value: d("90000") });
        assert_eq!(x.subject.unwrap().key, "bitcoin");
        assert_eq!(x.threshold, Some(d("90000")));
        assert_eq!(x.target_date, NaiveDate::from_ymd_opt(2026, 12, 31));
    }

    #[test]
    fn a_matchup_gets_both_participants_from_the_venues_outcomes() {
        let mut dictionary = Dictionary::seeded();
        let mut m = market("Falcons", Some("Falcons vs. Packers"));
        m.outcome_labels = vec!["Falcons".into(), "Packers".into()];
        m.entity = Some(VenueEntity { name: "Atlanta Falcons".into(), abbreviation: Some("atl".into()), image_url: None });
        m.market_kind = Some("moneyline".into());
        learn_from([&m], &mut dictionary);
        let x = extract(&m, &dictionary);
        assert_eq!(x.domain, Domain::Sports);
        let keys: Vec<&str> = x.participants.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, ["atlanta-falcons", "packers"]);
    }

    #[test]
    fn a_will_a_beat_b_question_names_both_sides() {
        let x = extract(
            &market("Will Nigeria (Super Falcons) beat Malawi by 2 or more goals in their WAFCON 2026 Group C opener?", None),
            &Dictionary::seeded(),
        );
        let keys: Vec<&str> = x.participants.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, ["malawi", "nigeria"]);
        assert_eq!(x.domain, Domain::Sports);
    }

    #[test]
    fn labels_are_classified_by_shape() {
        assert_eq!(classify_label("160-179"), LabelShape::Range { low: d("160"), high: d("179") });
        assert_eq!(classify_label("$2.0–$2.25T"), LabelShape::Range { low: d("2000000000000"), high: d("2250000000000") });
        assert_eq!(classify_label("15+"), LabelShape::Count(d("15")));
        assert_eq!(classify_label("25°C"), LabelShape::Count(d("25")));
        assert_eq!(classify_label("94-95°F"), LabelShape::Range { low: d("94"), high: d("95") });
        assert_eq!(classify_label("↓ 70,000"), LabelShape::Threshold { direction: Direction::Below, value: d("70000") });
        assert_eq!(classify_label("December 31"), LabelShape::Date(PartialDate { year: None, month: 12, day: 31 }));
        assert_eq!(classify_label("Yes"), LabelShape::YesNo);
        assert_eq!(classify_label("Marco Rubio"), LabelShape::Name);
    }

    #[test]
    fn keywords_match_whole_words_only() {
        let x = extract(&market("Yes", Some("Russia x Ukraine ceasefire agreement by...?")), &Dictionary::seeded());
        assert_eq!(x.domain, Domain::Politics, "'Ukraine' contains 'rain' but is not weather");
    }

    #[test]
    fn a_sports_category_outranks_political_words() {
        let mut m = market("Gunnar Henderson", Some("MLB: Runs Leader"));
        m.category = crate::models::venue::Category::from("sports");
        assert_eq!(extract(&m, &Dictionary::seeded()).domain, Domain::Sports);
    }

    #[test]
    fn a_candidate_under_a_nomination_event_is_politics() {
        let x = extract(&market("Marco Rubio", Some("Republican Presidential Nominee 2028")), &Dictionary::seeded());
        assert_eq!(x.domain, Domain::Politics);
        assert_eq!(x.label_shape, LabelShape::Name);
        assert_eq!(x.target_date, NaiveDate::from_ymd_opt(2028, 12, 31));
    }
}
