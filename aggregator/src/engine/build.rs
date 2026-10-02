//! Builds the event graph from the venues' markets: structure, extraction, matching, rating.
//!
//! 1. **Group** each venue's markets by the venue's own event, and infer the question structure:
//!    a matchup (named outcomes) is one *winner* question; mutually exclusive options (candidates,
//!    ranges - their YES prices sum to about 1) are one *multi* question; independent thresholds
//!    and deadlines ("↑ 90,000", "by March 31") are one yes/no question each.
//! 2. **Learn** option names and venue-attached participants into the entity dictionary, so any
//!    venue's text that names them is recognised.
//! 3. **Extract** every market (`extract`), and write each question as a full sentence.
//! 4. **Match** questions across venues (`matching`) and fold merged ones into one event.
//! 5. **Rate** every outcome's listings (`rating`).
//!
//! Pure: the same markets, overrides and clock always build the same graph.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::NaiveDate;

use chrono::Datelike;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use super::dictionary::{Dictionary, EntityKind, EntityRef};
use super::extract::{self, Direction, Domain, Extraction, LabelShape};
use super::graph::{EventGraph, EventNode, EventStatus, Listing, OutcomeNode, QuestionKind, QuestionNode, SourceRef, stable_id};
use super::headline;
use super::matching::{self, Candidate, MatchDecision, MatchOverrides, ScoreWeights};
use super::rating::{self, RatingWeights};
use super::text;
use crate::models::venue::VenueMarket;

/// Tunables for a build; defaults are the plan's.
#[derive(Debug, Clone, Default)]
pub struct BuildConfig {
    pub overrides: MatchOverrides,
    pub score_weights: ScoreWeights,
    pub rating_weights: RatingWeights,
}

pub struct BuildOutput {
    pub graph: EventGraph,
    /// Every cross-venue comparison worth recording, merged or not.
    pub decisions: Vec<MatchDecision>,
}

/// The YES prices of mutually exclusive options sum to about 1; independent yes/no markets
/// don't. Each option's ask carries some spread (long shots rarely trade below a cent or two), so
/// the upper bound grows with the number of options.
fn exclusive_price_sum(options: usize) -> std::ops::RangeInclusive<f64> {
    0.80..=(1.0 + 0.025 * options as f64).clamp(1.30, 1.75)
}

/// One question on one venue before matching.
struct Draft {
    key: String,
    kind: QuestionKind,
    label: String,
    text: String,
    line: Option<Decimal>,
    close_time: i64,
    outcomes: Vec<DraftOutcome>,
    /// For matching: one per question, or one per option of a multi question.
    candidates: Vec<Candidate>,
    confidence: f32,
    domain: Domain,
    entities: Vec<EntityRef>,
    /// The asset a price question is about, and the day it settles on - an event whose questions
    /// all share both is one day of that asset's price (see `group_related`).
    price_day: Option<(EntityRef, NaiveDate)>,
}

struct DraftOutcome {
    label: String,
    /// The option's key (for multi) or participant key (for winner), to align merged outcomes.
    align_key: String,
    entity: Option<EntityRef>,
    listings: Vec<Listing>,
}

pub fn build_graph(markets: &[&VenueMarket], config: &BuildConfig, routable: impl Fn(&str) -> bool, now: i64) -> BuildOutput {
    // ── 1. Group by venue event ─────────────────────────────────────────────────────────────
    let mut groups: BTreeMap<(String, String), Vec<&VenueMarket>> = BTreeMap::new();
    for market in markets.iter().filter(|m| m.is_bettable(now)) {
        let event_id = market.event.as_ref().map_or(market.venue_market_id.clone(), |e| e.event_id.clone());
        groups.entry((market.venue_id.clone(), event_id)).or_default().push(market);
    }
    for group in groups.values_mut() {
        group.sort_by(|a, b| a.venue_market_id.cmp(&b.venue_market_id));
    }

    // ── 2. Learn entities ───────────────────────────────────────────────────────────────────
    let mut dictionary = Dictionary::seeded();
    extract::learn_from(markets.iter().copied(), &mut dictionary);
    for group in groups.values() {
        if structure_of(group) == Structure::Exclusive {
            for market in group {
                if extract::classify_label(&market.title) == LabelShape::Name && dictionary.find_all(&market.title).is_empty() {
                    dictionary.learn_participant(&market.title, None, EntityKind::Person);
                }
            }
        }
    }

    // ── 3. Extract and draft questions ──────────────────────────────────────────────────────
    let mut drafts: Vec<Draft> = Vec::new();
    let mut events: Vec<EventDraft> = Vec::new();
    for ((venue_id, event_id), group) in &groups {
        let before = drafts.len();
        draft_questions(venue_id, event_id, group, &dictionary, &mut drafts);
        events.push(EventDraft { venue_id: venue_id.clone(), event_id: event_id.clone(), markets: group.clone(), questions: (before..drafts.len()).collect() });
    }

    // ── 4. Match across venues ──────────────────────────────────────────────────────────────
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut owner: Vec<(usize, Option<usize>)> = Vec::new(); // (draft, option index)
    for (d, draft) in drafts.iter().enumerate() {
        for (c, candidate) in draft.candidates.iter().enumerate() {
            candidates.push(candidate.clone());
            owner.push((d, (draft.kind == QuestionKind::Multi).then_some(c)));
        }
    }
    let (roots, decisions) = matching::resolve(&candidates, &config.score_weights, &config.overrides);
    let mut sets: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, root) in roots.iter().enumerate() {
        sets.entry(*root).or_default().push(i);
    }

    let mut absorbed_into: HashMap<usize, usize> = HashMap::new(); // donor draft -> host draft
    let mut rules_differ: HashMap<usize, bool> = HashMap::new();
    for members in sets.values().filter(|m| m.len() > 1) {
        // Host: a multi-choice question if one is involved (a binary about one option folds into
        // it), else the question from the richer venue event.
        let host = *members
            .iter()
            .max_by_key(|&&i| {
                let (d, option) = owner[i];
                (option.is_some(), events.iter().find(|e| e.questions.contains(&d)).map_or(0, |e| e.questions.len()), std::cmp::Reverse(d))
            })
            .unwrap();
        let (host_draft, host_option) = owner[host];
        let exact = decisions
            .iter()
            .any(|dec| dec.merged && dec.reason != "score" && (dec.left == candidates[host].listing_ids || dec.right == candidates[host].listing_ids));
        for &member in members.iter().filter(|&&m| m != host) {
            let (donor_draft, _) = owner[member];
            if donor_draft == host_draft || absorbed_into.contains_key(&donor_draft) {
                continue;
            }
            let donor_outcomes = std::mem::take(&mut drafts[donor_draft].outcomes);
            let host = &mut drafts[host_draft];
            match host_option {
                // A binary about one option: its YES backs that option.
                Some(option) => {
                    let yes: Vec<Listing> = donor_outcomes.into_iter().filter(|o| o.label == "Yes").flat_map(|o| o.listings).collect();
                    host.outcomes[option].listings.extend(yes);
                }
                None => {
                    for donor in donor_outcomes {
                        if let Some(target) = host.outcomes.iter_mut().find(|o| o.align_key == donor.align_key) {
                            target.listings.extend(donor.listings);
                        }
                    }
                }
            }
            absorbed_into.insert(donor_draft, host_draft);
            *rules_differ.entry(host_draft).or_default() |= !exact;
        }
    }

    // ── 5. Assemble events, rate listings ───────────────────────────────────────────────────
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut nodes: Vec<EventNode> = Vec::new();
    // Donor events that lost every question are folded into their host's event.
    let event_of_draft: HashMap<usize, usize> =
        events.iter().enumerate().flat_map(|(e, ev)| ev.questions.iter().map(move |q| (*q, e))).collect();
    let mut extra_sources: HashMap<usize, Vec<SourceRef>> = HashMap::new();
    for event in &events {
        let remaining = event.questions.iter().filter(|q| !absorbed_into.contains_key(q)).count();
        if remaining == 0 && !event.questions.is_empty() {
            let host_event = event_of_draft[&absorbed_into[&event.questions[0]]];
            aliases.insert(event_id(&event.venue_id, &event.event_id), event_id(&events[host_event].venue_id, &events[host_event].event_id));
            extra_sources.entry(host_event).or_default().push(source_of(event));
        }
    }

    let mut drafts: Vec<Option<Draft>> = drafts.into_iter().map(Some).collect();
    let mut price_days: Vec<Option<(EntityRef, NaiveDate)>> = Vec::new();
    for (e, event) in events.iter().enumerate() {
        let mut questions: Vec<QuestionNode> = Vec::new();
        let mut confidences = Vec::new();
        let mut domains: Vec<Domain> = Vec::new();
        let mut entities: Vec<EntityRef> = Vec::new();
        let mut days: Vec<Option<(EntityRef, NaiveDate)>> = Vec::new();
        for &q in &event.questions {
            if absorbed_into.contains_key(&q) {
                continue;
            }
            let draft = drafts[q].take().expect("each draft is assembled once");
            days.push(draft.price_day.clone());
            confidences.push(draft.confidence);
            domains.push(draft.domain);
            for entity in &draft.entities {
                if !entities.contains(entity) {
                    entities.push(entity.clone());
                }
            }
            let mut outcomes: Vec<OutcomeNode> = draft
                .outcomes
                .into_iter()
                .map(|o| OutcomeNode { id: stable_id("o", &format!("{}:{}", draft.key, o.align_key)), label: o.label, entity: o.entity, listings: o.listings })
                .collect();
            for outcome in &mut outcomes {
                rating::rate_outcome(&mut outcome.listings, &config.rating_weights, &routable);
            }
            questions.push(QuestionNode {
                id: stable_id("q", &draft.key),
                kind: draft.kind,
                label: draft.label,
                text: draft.text,
                line: draft.line,
                close_time: draft.close_time,
                outcomes,
                rules_differ: rules_differ.get(&q).copied().unwrap_or(false),
            });
        }
        if questions.is_empty() {
            continue;
        }
        questions.sort_by(|a, b| b.volume().cmp(&a.volume()).then(a.close_time.cmp(&b.close_time)));

        let mut sources = vec![source_of(event)];
        sources.extend(extra_sources.remove(&e).unwrap_or_default());
        let venue_count = {
            let mut venues: Vec<&str> = questions.iter().flat_map(|q| q.venue_ids()).collect();
            venues.sort();
            venues.dedup();
            venues.len()
        };
        nodes.push(assemble_event(event, questions, sources, venue_count, &confidences, &domains, entities, now));
        // One asset's price on one day only when every question agrees on both.
        let first = days.first().cloned().flatten();
        let same_day = first.filter(|(s, d)| days.iter().all(|x| x.as_ref().is_some_and(|(xs, xd)| xs.key == s.key && xd == d)));
        price_days.push(same_day);
    }

    // ── 6. Group related events ──────────────────────────────────────────────────────────────
    let mut nodes = group_related(nodes, &price_days, &mut aliases, now);
    for node in &mut nodes {
        node.context = context_line(node);
    }
    nodes.sort_by(|a, b| b.volume_usdc.cmp(&a.volume_usdc).then(a.close_time.cmp(&b.close_time)));

    BuildOutput { graph: EventGraph::new(nodes, aliases, now), decisions }
}

/// Why two events belong together: the same asset's price on the same day, or the same series -
/// titles that differ only in their dates and numbers ("Will China invade Taiwan by Sep 30,
/// 2026?", "…by Jun 30, 2027?"). Grouping never merges questions: each keeps its own listings.
fn group_key(node: &EventNode, price_day: Option<&(EntityRef, NaiveDate)>) -> Option<String> {
    if let Some((subject, day)) = price_day {
        return Some(format!("px:{}:{day}", subject.key));
    }
    let title = &node.sources.first()?.title;
    let template = headline::series_template(title);
    // A template of placeholders and one word ("<n> <d>") says nothing about the subject.
    let words = template.split(' ').filter(|w| !w.starts_with('<') && w.chars().any(char::is_alphabetic)).count();
    (words >= 2).then(|| format!("series:{}:{template}", node.category))
}

/// Folds related events into one event each (`group_key`). Singles pass through unchanged; a
/// folded event's old id keeps resolving to the group.
fn group_related(
    nodes: Vec<EventNode>,
    price_days: &[Option<(EntityRef, NaiveDate)>],
    aliases: &mut HashMap<String, String>,
    now: i64,
) -> Vec<EventNode> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let keys: Vec<Option<String>> = nodes.iter().zip(price_days).map(|(n, d)| group_key(n, d.as_ref())).collect();
    for (i, key) in keys.iter().enumerate() {
        if let Some(key) = key {
            groups.entry(key.clone()).or_default().push(i);
        }
    }
    let grouped: HashSet<usize> = groups.values().filter(|m| m.len() > 1).flatten().copied().collect();

    let mut slots: Vec<Option<EventNode>> = nodes.into_iter().map(Some).collect();
    let mut out: Vec<EventNode> = (0..slots.len()).filter(|i| !grouped.contains(i)).map(|i| slots[i].take().unwrap()).collect();
    for (key, indices) in groups.into_iter().filter(|(_, m)| m.len() > 1) {
        let price_day = price_days[indices[0]].clone();
        let members: Vec<EventNode> = indices.iter().map(|&i| slots[i].take().expect("an event is in one group")).collect();
        let merged = merge_events(&key, members, price_day, now);
        for source_id in merged.sources.iter().map(|s| event_id(&s.venue_id, &s.venue_event_id)) {
            if source_id != merged.id {
                aliases.insert(source_id, merged.id.clone());
            }
        }
        out.push(merged);
    }
    // An alias that pointed at an event now folded into a group points at the group.
    let folded: HashMap<String, String> = aliases.iter().filter(|(_, to)| aliases.contains_key(*to)).map(|(from, to)| (from.clone(), aliases[to].clone())).collect();
    aliases.extend(folded);
    out
}

/// One event from several related ones. Questions are kept whole, in date order then by line;
/// the busiest member lends the image, description and category.
fn merge_events(key: &str, mut members: Vec<EventNode>, price_day: Option<(EntityRef, NaiveDate)>, now: i64) -> EventNode {
    members.sort_by_key(|m| std::cmp::Reverse(m.volume_usdc));
    let lead = &members[0];
    let titles: Vec<&str> = members.iter().flat_map(|m| m.sources.iter().map(|s| s.title.as_str())).collect();
    let mut questions: Vec<QuestionNode> = members.iter().flat_map(|m| m.questions.iter().cloned()).collect();
    questions.sort_by(|a, b| a.close_time.cmp(&b.close_time).then(a.line.cmp(&b.line)).then(a.id.cmp(&b.id)));

    let title = match &price_day {
        Some((subject, day)) => format!("{} price on {}", subject.name, day.format("%B %-d, %Y")),
        None => {
            let shared = headline::common_headline(&titles);
            let years: Vec<i32> = questions.iter().filter(|q| q.close_time > 0).filter_map(|q| text::date_of(q.close_time)).map(|d| d.year()).collect();
            let (first, last) = (years.iter().min(), years.iter().max());
            match (first, last) {
                // The years the series spans are context the shared words dropped.
                (Some(a), Some(b)) if a != b && !shared.contains(&a.to_string()) => format!("{shared} ({a}–{b})"),
                _ => shared,
            }
        }
    };
    let title = if title.split_whitespace().count() >= 2 { title } else { lead.title.clone() };

    let mut tags: Vec<String> = Vec::new();
    let mut entities: Vec<EntityRef> = Vec::new();
    for member in &members {
        for tag in &member.tags {
            if !tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                tags.push(tag.clone());
            }
        }
        for entity in &member.entities {
            if !entities.iter().any(|e| e.key == entity.key) {
                entities.push(entity.clone());
            }
        }
    }
    let close_time = questions.iter().map(|q| q.close_time).filter(|t| *t > 0).min().unwrap_or(0);
    let start_time = members.iter().filter_map(|m| m.start_time).min();
    let status = if members.iter().any(|m| m.status == EventStatus::Live) {
        EventStatus::Live
    } else if members.iter().all(|m| m.status == EventStatus::Closed) || (close_time > 0 && close_time <= now) {
        EventStatus::Closed
    } else {
        EventStatus::Upcoming
    };
    let venue_count = {
        let mut venues: Vec<&str> = questions.iter().flat_map(|q| q.venue_ids()).collect();
        venues.sort();
        venues.dedup();
        venues.len()
    };
    let subtitles: HashSet<&Option<String>> = members.iter().map(|m| &m.subtitle).collect();
    EventNode {
        id: stable_id("evt", &format!("group:{key}")),
        slug: text::slug(&title),
        title,
        subtitle: if subtitles.len() == 1 { lead.subtitle.clone() } else { None },
        context: String::new(),
        description: lead.description.clone(),
        category: lead.category.clone(),
        subcategory: lead.subcategory.clone(),
        domain: lead.domain.clone(),
        tags,
        entities,
        image_url: members.iter().find_map(|m| m.image_url.clone()),
        start_time,
        close_time,
        status,
        volume_usdc: members.iter().map(|m| m.volume_usdc).sum(),
        venue_count,
        question_count: questions.len(),
        sources: members.iter().flat_map(|m| m.sources.iter().cloned()).collect(),
        metadata_confidence: (members.iter().map(|m| m.metadata_confidence).sum::<f32>() / members.len() as f32 * 100.0).round() / 100.0,
        questions,
    }
}

/// What an event covers, in one line: "Politics · 3 questions on Jupiter · closes Sep 30, 2026 –
/// Dec 31, 2027".
fn context_line(event: &EventNode) -> String {
    let mut venues: Vec<String> = event.questions.iter().flat_map(|q| q.venue_ids()).map(venue_name).collect();
    venues.dedup();
    let venues = match venues.as_slice() {
        [] => String::new(),
        [one] => format!(" on {one}"),
        [init @ .., last] => format!(" on {} and {last}", init.join(", ")),
    };
    let n = event.questions.len();
    let questions = format!("{n} question{}", if n == 1 { "" } else { "s" });
    let closes: Vec<i64> = event.questions.iter().map(|q| q.close_time).filter(|t| *t > 0).collect();
    let day = |t: i64| text::date_of(t).map(|d| d.format("%b %-d, %Y").to_string()).unwrap_or_default();
    let window = match (closes.iter().min(), closes.iter().max()) {
        (Some(&a), Some(&b)) if day(a) != day(b) => format!(" · closes {} – {}", day(a), day(b)),
        (Some(&a), _) => format!(" · closes {}", day(a)),
        _ => String::new(),
    };
    let mut category = event.category.clone();
    if let Some(first) = category.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    format!("{category} · {questions}{venues}{window}")
}

fn venue_name(venue_id: &str) -> String {
    let mut name = venue_id.to_string();
    if let Some(first) = name.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    name
}


struct EventDraft<'a> {
    venue_id: String,
    event_id: String,
    markets: Vec<&'a VenueMarket>,
    questions: Vec<usize>,
}

fn event_id(venue_id: &str, venue_event_id: &str) -> String {
    stable_id("evt", &format!("{venue_id}:{venue_event_id}"))
}

fn source_of(event: &EventDraft) -> SourceRef {
    let first = event.markets.first();
    let title = first.and_then(|m| m.event.as_ref()).map_or_else(|| first.map_or("", |m| m.title.as_str()), |e| e.title.as_str());
    SourceRef {
        venue_id: event.venue_id.clone(),
        venue_event_id: event.event_id.clone(),
        title: title.trim().to_string(),
        venue_category: event.markets.first().map(|m| m.venue_category.clone()).unwrap_or_default(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Structure {
    /// A market with two named outcomes (a matchup) - one winner question per market group.
    Matchup,
    /// Several yes/no markets that are options of one choice.
    Exclusive,
    /// Several yes/no markets that are separate questions.
    Independent,
    /// One yes/no market.
    Single,
}

fn named_outcomes(market: &VenueMarket) -> Vec<String> {
    market
        .outcome_labels
        .iter()
        .filter(|o| !matches!(text::fold(o).as_str(), "yes" | "no"))
        .cloned()
        .collect()
}

fn structure_of(group: &[&VenueMarket]) -> Structure {
    if group.iter().any(|m| named_outcomes(m).len() >= 2) {
        return Structure::Matchup;
    }
    if group.len() == 1 {
        return Structure::Single;
    }
    let shapes: Vec<LabelShape> = group.iter().map(|m| extract::classify_label(&m.title)).collect();
    // Buckets are exclusive, including the open-ended ones at either edge ("≤20°C", "30°C or
    // higher") that read as thresholds on their own.
    let buckets = shapes.iter().filter(|s| matches!(s, LabelShape::Range { .. } | LabelShape::Count(_))).count();
    let edges = shapes.iter().filter(|s| matches!(s, LabelShape::Threshold { .. })).count();
    if buckets >= 2 && buckets + edges == shapes.len() && edges <= 2 {
        return Structure::Exclusive;
    }
    if shapes.iter().any(|s| matches!(s, LabelShape::Threshold { .. } | LabelShape::Date(_))) {
        return Structure::Independent;
    }
    let price_sum: f64 = group.iter().filter_map(|m| m.yes_price.as_deref()?.parse::<f64>().ok()).sum();
    if exclusive_price_sum(group.len()).contains(&price_sum) { Structure::Exclusive } else { Structure::Independent }
}

fn listing(market: &VenueMarket, side: &str) -> Listing {
    let price = match side {
        "no" => market.no_price.as_deref(),
        _ => market.yes_price.as_deref(),
    };
    Listing {
        venue_id: market.venue_id.clone(),
        venue_market_id: market.venue_market_id.clone(),
        venue_event_id: market.event.as_ref().map(|e| e.event_id.clone()),
        side: side.to_string(),
        price: price.and_then(|p| p.parse().ok()),
        volume_usdc: market.liquidity,
        close_time: market.close_time,
        tradable: !market.display_only,
        rules: market.rules.clone(),
        rating: None,
    }
}

fn candidate(market: &VenueMarket, extraction: &Extraction, option: Option<String>, winner: bool, ids: Vec<String>) -> Candidate {
    let full = format!("{} {}", market.event.as_ref().map_or("", |e| e.title.as_str()), market.title);
    Candidate {
        listing_ids: ids,
        venue_id: market.venue_id.clone(),
        style: matching::settle_style(&full, &extraction.label_shape),
        option,
        winner,
        extraction: extraction.clone(),
    }
}

fn draft_questions(venue_id: &str, event_id: &str, group: &[&VenueMarket], dictionary: &Dictionary, drafts: &mut Vec<Draft>) {
    let event_title = group[0].event.as_ref().map_or(group[0].title.as_str(), |e| e.title.as_str()).trim().to_string();
    let base_key = format!("{venue_id}:{event_id}");
    match structure_of(group) {
        Structure::Matchup => {
            // One winner question per distinct outcome set and market kind (moneyline, spread…).
            let mut by_shape: BTreeMap<(Vec<String>, Option<String>), Vec<&VenueMarket>> = BTreeMap::new();
            for market in group {
                let mut named = named_outcomes(market);
                if named.len() < 2 {
                    named = vec![market.title.clone()];
                }
                by_shape.entry((named, market.market_kind.clone())).or_default().push(market);
            }
            for ((named, kind), markets) in by_shape {
                let extraction = extract::extract(markets[0], dictionary);
                let ids = matching::listing_ids(venue_id, markets.iter().map(|m| m.venue_market_id.as_str()));
                let mut outcomes: Vec<DraftOutcome> = named
                    .iter()
                    .map(|label| {
                        let entity = dictionary.lookup(label).or_else(|| {
                            extraction.participants.iter().find(|p| text::fold(&p.name) == text::fold(label)).cloned()
                        });
                        let align_key = entity.as_ref().map_or_else(|| text::slug(label), |e| e.key.clone());
                        DraftOutcome { label: label.clone(), align_key, entity, listings: vec![] }
                    })
                    .collect();
                for market in &markets {
                    let own = text::fold(&market.title);
                    for outcome in outcomes.iter_mut() {
                        if text::fold(&outcome.label) == own {
                            outcome.listings.push(listing(market, "yes"));
                        } else if named.len() == 2 {
                            // In a two-way market, NO on one side is YES on the other.
                            outcome.listings.push(listing(market, "no"));
                        }
                    }
                }
                let label = match kind.as_deref() {
                    Some("moneyline") | None => "Who wins?".to_string(),
                    Some(other) => format!("{} ({other})", "Who wins?"),
                };
                let up_down = is_up_down(&named);
                // In the venue's own order and spelling ("G2 Esports vs TYLOO", not sorted keys).
                let text = match named.as_slice() {
                    _ if up_down => up_down_sentence(&event_title),
                    [a, b] => format!("Who will win {a} vs {b}?"),
                    _ => question_sentence(&event_title),
                };
                let key = format!("{base_key}:{}", ids.join(","));
                drafts.push(Draft {
                    candidates: vec![candidate(markets[0], &extraction, None, true, ids)],
                    key,
                    kind: QuestionKind::Winner,
                    label,
                    text,
                    line: None,
                    close_time: markets.iter().map(|m| m.close_time).filter(|t| *t > 0).min().unwrap_or(0),
                    outcomes,
                    confidence: extraction.confidence,
                    domain: extraction.domain,
                    entities: extraction.entities.clone(),
                    price_day: if up_down { price_day(&extraction) } else { None },
                });
            }
        }
        Structure::Exclusive => {
            let extractions: Vec<Extraction> = group.iter().map(|m| extract::extract(m, dictionary)).collect();
            let ids = matching::listing_ids(venue_id, group.iter().map(|m| m.venue_market_id.as_str()));
            let mut outcomes = Vec::new();
            let mut candidates = Vec::new();
            let mut entities = Vec::new();
            for (market, extraction) in group.iter().zip(&extractions) {
                let entity = dictionary.lookup(&market.title);
                let align_key = entity.as_ref().map_or_else(|| text::slug(&market.title), |e| e.key.clone());
                outcomes.push(DraftOutcome { label: market.title.clone(), align_key: align_key.clone(), entity: entity.clone(), listings: vec![listing(market, "yes")] });
                candidates.push(candidate(market, extraction, Some(align_key), false, matching::listing_ids(venue_id, [market.venue_market_id.as_str()])));
                entities.extend(entity);
            }
            let confidence = extractions.iter().map(|x| x.confidence).sum::<f32>() / extractions.len() as f32;
            drafts.push(Draft {
                key: format!("{base_key}:{}", ids.join(",")),
                kind: QuestionKind::Multi,
                label: "Which outcome?".to_string(),
                text: question_sentence(&event_title),
                line: None,
                close_time: group.iter().map(|m| m.close_time).filter(|t| *t > 0).min().unwrap_or(0),
                outcomes,
                candidates,
                confidence,
                domain: extractions[0].domain,
                entities,
                // Price buckets ("$110,000-$112,000"), not a list of names.
                price_day: extractions
                    .iter()
                    .all(|x| matches!(x.label_shape, LabelShape::Range { .. } | LabelShape::Threshold { .. } | LabelShape::Count(_)))
                    .then(|| price_day(&extractions[0]))
                    .flatten(),
            });
        }
        Structure::Independent | Structure::Single => {
            for market in group {
                let extraction = extract::extract(market, dictionary);
                let ids = matching::listing_ids(venue_id, [market.venue_market_id.as_str()]);
                let kind = match (&extraction.label_shape, extraction.threshold.is_some() && extraction.subject.is_some()) {
                    (LabelShape::Threshold { .. }, _) | (_, true) => QuestionKind::Threshold,
                    (LabelShape::Date(_), _) => QuestionKind::Deadline,
                    _ => QuestionKind::Binary,
                };
                let under_event = market.event.as_ref().is_some_and(|e| text::fold(&e.title) != text::fold(&market.title));
                let label = if under_event { market.title.clone() } else { "Yes or no?".to_string() };
                let text = binary_sentence(market, &extraction, under_event);
                drafts.push(Draft {
                    key: format!("{base_key}:{}", ids.join(",")),
                    kind,
                    label,
                    text,
                    line: extraction.threshold,
                    close_time: market.close_time,
                    outcomes: vec![
                        DraftOutcome { label: "Yes".into(), align_key: "yes".into(), entity: None, listings: vec![listing(market, "yes")] },
                        DraftOutcome { label: "No".into(), align_key: "no".into(), entity: None, listings: vec![listing(market, "no")] },
                    ],
                    candidates: vec![candidate(market, &extraction, None, false, ids)],
                    confidence: extraction.confidence,
                    domain: extraction.domain,
                    entities: extraction.entities.clone(),
                    // A level in a direction ("fall to $270"), not a stray number in the text.
                    price_day: (extraction.threshold.is_some() && extraction.direction.is_some()).then(|| price_day(&extraction)).flatten(),
                });
            }
        }
    }
}

fn is_up_down(named: &[String]) -> bool {
    let mut folded: Vec<String> = named.iter().map(|n| text::fold(n)).collect();
    folded.sort();
    folded == ["down", "up"]
}

/// "Bitcoin Up or Down - September 24, 2:30PM-2:35PM ET" → "Will Bitcoin go up or down, September
/// 24, 2:30PM-2:35PM ET?"; "SPY Opens Up or Down on September 24?" → "Will SPY open up or down on
/// September 24?"
fn up_down_sentence(event_title: &str) -> String {
    static UP_DOWN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^(.+?)(?:\s+(opens|closes))?\s+up\s+or\s+down(?:\s*([-–:,])\s*|\s+(on|at|in|by)\s+|\s*)(.*?)\??$").unwrap()
    });
    let Some(c) = UP_DOWN.captures(event_title.trim()) else { return question_sentence(event_title) };
    let verb = match c.get(2).map(|v| v.as_str().to_lowercase()) {
        Some(v) if v == "opens" => "open",
        Some(v) if v == "closes" => "close",
        _ => "go",
    };
    let when = c.get(5).map(|w| w.as_str().trim()).filter(|w| !w.is_empty());
    match (when, c.get(4)) {
        (Some(when), Some(preposition)) => format!("Will {} {verb} up or down {} {when}?", &c[1], preposition.as_str().to_lowercase()),
        (Some(when), None) => format!("Will {} {verb} up or down, {when}?", &c[1]),
        (None, _) => format!("Will {} {verb} up or down?", &c[1]),
    }
}

/// The (asset, day) of a price question, when extraction found both.
fn price_day(x: &Extraction) -> Option<(EntityRef, NaiveDate)> {
    let subject = x.subject.clone().filter(|s| matches!(s.kind, EntityKind::Asset | EntityKind::Stock | EntityKind::Commodity | EntityKind::Index))?;
    Some((subject, x.target_date?))
}

/// A title as a question: kept if it already is one, otherwise given a question mark only when
/// it reads as one ("Will …", "Who …").
fn question_sentence(title: &str) -> String {
    let collapsed = title.split_whitespace().collect::<Vec<_>>().join(" ").replace(" ?", "?");
    let title = collapsed.trim();
    if title.ends_with('?') {
        return title.to_string();
    }
    let first = text::fold(title.split_whitespace().next().unwrap_or(""));
    if ["will", "who", "what", "which", "how", "when", "is", "are", "does", "do", "can"].contains(&first.as_str()) {
        format!("{title}?")
    } else {
        title.to_string()
    }
}

fn format_level(value: Decimal, subject: Option<&EntityRef>) -> String {
    let money = subject.is_some_and(|s| matches!(s.kind, EntityKind::Asset | EntityKind::Stock | EntityKind::Commodity | EntityKind::Index));
    let rounded = value.round_dp(2).normalize();
    let whole = rounded.trunc().to_i128().unwrap_or(0);
    let mut grouped = String::new();
    for (i, c) in whole.abs().to_string().chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let mut out: String = grouped.chars().rev().collect();
    let fraction = rounded.fract().abs();
    if !fraction.is_zero() {
        out.push_str(&fraction.to_string()[1..]);
    }
    if money { format!("${out}") } else { out }
}

/// A yes/no question as one sentence. A venue that already wrote the question (Panta) keeps its
/// wording; a label under an event question (Jupiter's "↑ 90,000") becomes "Will Bitcoin reach
/// $90,000 by Dec 31, 2026?" when the extraction supports it, else "{event} — {label}".
fn binary_sentence(market: &VenueMarket, x: &Extraction, under_event: bool) -> String {
    if !under_event {
        return question_sentence(&market.title);
    }
    let event_title = market.event.as_ref().map_or("", |e| e.title.as_str());
    // A fill-in-the-blank title ("Bitcoin above ___ on September 24?") takes the label in the blank.
    if let Some(blank) = BLANK.find(event_title) {
        let fill = market.title.trim().trim_start_matches(['↑', '↓', '>', '<']).trim();
        return question_sentence(&format!("{}{fill}{}", &event_title[..blank.start()], &event_title[blank.end()..]));
    }
    if let (LabelShape::Threshold { direction, value }, Some(subject)) = (&x.label_shape, &x.subject) {
        let verb = match direction {
            Direction::Above => "reach",
            Direction::Below => "fall to",
        };
        let by = x.target_date.map(|d| format!(" by {}", d.format("%b %-d, %Y"))).unwrap_or_default();
        return format!("Will {} {verb} {}{by}?", subject.name, format_level(*value, Some(subject)));
    }
    if let LabelShape::Date(_) = x.label_shape
        && let Some(date) = x.target_date
    {
        // "Netanyahu out by…?" + "March 31" → "Netanyahu out by Mar 31, 2027?"
        let mut stem = event_title.trim();
        loop {
            let trimmed = stem.trim_end_matches(['?', '.', '…', ' ']).trim_end();
            let trimmed = ["by", "before", "through", "until", "on", "in"]
                .iter()
                .find_map(|w| trimmed.strip_suffix(w).filter(|rest| rest.ends_with(' ')))
                .map_or(trimmed, str::trim_end);
            if trimmed == stem {
                break;
            }
            stem = trimmed;
        }
        let joiner = if text::fold(event_title).contains("through") { "through" } else { "by" };
        return format!("{stem} {joiner} {}?", date.format("%b %-d, %Y"));
    }
    if matches!(x.label_shape, LabelShape::YesNo) {
        return question_sentence(event_title);
    }
    format!("{} — {}", event_title.trim_end_matches('?'), market.title)
}

static BLANK: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"_{2,}").unwrap());

#[allow(clippy::too_many_arguments)]
fn assemble_event(
    event: &EventDraft,
    questions: Vec<QuestionNode>,
    sources: Vec<SourceRef>,
    venue_count: usize,
    confidences: &[f32],
    domains: &[Domain],
    entities: Vec<EntityRef>,
    now: i64,
) -> EventNode {
    let first = event.markets[0];
    let venue_event = first.event.clone().unwrap_or_default();
    let venue_title = if venue_event.title.trim().is_empty() { first.title.trim() } else { venue_event.title.trim() };
    // The venue's title is often a question; the event's is a headline (`headline`).
    let title = Some(headline::headline(venue_title)).filter(|t| t.split_whitespace().count() >= 2).unwrap_or_else(|| venue_title.to_string());
    let close_time = questions.iter().map(|q| q.close_time).filter(|t| *t > 0).min().unwrap_or(0);
    let start_time = venue_event.start_time.filter(|t| *t > 0);
    let status = match start_time {
        _ if close_time > 0 && close_time <= now => EventStatus::Closed,
        Some(start) if start <= now => EventStatus::Live,
        _ => EventStatus::Upcoming,
    };
    let volume_usdc = venue_event.volume_usdc.unwrap_or_else(|| questions.iter().map(|q| q.volume()).sum());
    let mut domain_counts: BTreeMap<Domain, usize> = BTreeMap::new();
    for d in domains {
        *domain_counts.entry(*d).or_default() += 1;
    }
    let domain = domain_counts.into_iter().max_by_key(|(_, n)| *n).map_or(Domain::Other, |(d, _)| d);
    let description = venue_event.description.clone().or_else(|| {
        first.rules.as_ref().and_then(|r| r.split(['.', '\n']).map(str::trim).find(|s| s.len() > 20)).map(|s| format!("{s}."))
    });
    let mut tags = venue_event.tags.clone();
    for tag in [venue_event.league.clone(), venue_event.subcategory.clone()].into_iter().flatten() {
        if !tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
            tags.push(tag);
        }
    }
    let confidence = if confidences.is_empty() { 0.0 } else { confidences.iter().sum::<f32>() / confidences.len() as f32 };
    EventNode {
        id: event_id(&event.venue_id, &event.event_id),
        slug: venue_event.slug.clone().unwrap_or_else(|| text::slug(&title)),
        title,
        subtitle: venue_event.subtitle.clone(),
        context: String::new(),
        description,
        category: first.category.as_str().to_string(),
        subcategory: venue_event.league.clone().or(venue_event.subcategory.clone()),
        domain: domain.as_str().to_string(),
        tags,
        entities,
        image_url: venue_event.image_url.clone(),
        start_time,
        close_time,
        status,
        volume_usdc,
        venue_count,
        question_count: questions.len(),
        sources,
        metadata_confidence: (confidence * 100.0).round() / 100.0,
        questions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::venue::{Category, VenueEntity, VenueEvent};

    const NOW: i64 = 1_789_000_000;

    fn market(venue: &str, id: &str, title: &str, event: Option<(&str, &str)>, yes: &str) -> VenueMarket {
        VenueMarket {
            venue_id: venue.into(),
            venue_market_id: id.into(),
            title: title.into(),
            event: event.map(|(eid, t)| VenueEvent { event_id: eid.into(), title: t.into(), ..Default::default() }),
            category: Category::from("crypto"),
            venue_category: "crypto".into(),
            close_time: NOW + 86_400 * 30,
            resolution_time: NOW + 86_400 * 31,
            yes_price: Some(yes.into()),
            no_price: Some((Decimal::ONE - yes.parse::<Decimal>().unwrap()).to_string()),
            liquidity: Some(Decimal::from(1000)),
            ..Default::default()
        }
    }

    fn build(markets: &[VenueMarket]) -> BuildOutput {
        let refs: Vec<&VenueMarket> = markets.iter().collect();
        build_graph(&refs, &BuildConfig::default(), |_| true, NOW)
    }

    #[test]
    fn independent_thresholds_become_one_question_each_with_a_full_sentence() {
        let event = Some(("e1", "What price will Bitcoin hit by December 31, 2026?"));
        let out = build(&[market("jupiter", "a", "↑ 90,000", event, "0.4"), market("jupiter", "b", "↑ 100,000", event, "0.2")]);
        let e = &out.graph.events[0];
        assert_eq!(e.questions.len(), 2);
        let texts: Vec<&str> = e.questions.iter().map(|q| q.text.as_str()).collect();
        assert!(texts.contains(&"Will Bitcoin reach $90,000 by Dec 31, 2026?"), "{texts:?}");
        assert!(e.questions.iter().all(|q| q.kind == QuestionKind::Threshold));
        assert_eq!(e.domain, "crypto");
    }

    #[test]
    fn exclusive_options_become_one_multi_question() {
        let event = Some(("e1", "Republican Presidential Nominee 2028"));
        let out = build(&[
            market("jupiter", "a", "J.D. Vance", event, "0.55"),
            market("jupiter", "b", "Marco Rubio", event, "0.30"),
            market("jupiter", "c", "Tucker Carlson", event, "0.10"),
        ]);
        let q = &out.graph.events[0].questions[0];
        assert_eq!(q.kind, QuestionKind::Multi);
        assert_eq!(q.outcomes.len(), 3);
    }

    #[test]
    fn a_matchup_is_one_winner_question_with_both_ways_to_back_each_side() {
        let mut falcons = market("jupiter", "f", "Falcons", Some(("g1", "Falcons vs. Packers")), "0.31");
        falcons.outcome_labels = vec!["Falcons".into(), "Packers".into()];
        falcons.entity = Some(VenueEntity { name: "Atlanta Falcons".into(), abbreviation: Some("atl".into()), image_url: None });
        falcons.market_kind = Some("moneyline".into());
        let out = build(&[falcons]);
        let q = &out.graph.events[0].questions[0];
        assert_eq!(q.kind, QuestionKind::Winner);
        assert_eq!(q.text, "Who will win Falcons vs Packers?");
        let packers = q.outcomes.iter().find(|o| o.label == "Packers").unwrap();
        assert_eq!(packers.listings[0].side, "no", "NO on the Falcons market backs the Packers");
    }

    #[test]
    fn a_binary_about_one_option_folds_into_that_option_across_venues() {
        let event = Some(("e1", "Republican Presidential Nominee 2028"));
        let mut panta = market("panta", "p", "Marco Rubio to win the 2028 Presidential nomination of the Republican Party", None, "0.33");
        panta.event = Some(VenueEvent { event_id: "p".into(), title: panta.title.clone(), ..Default::default() });
        panta.category = Category::from("sports");
        let markets = vec![
            market("jupiter", "a", "J.D. Vance", event, "0.55"),
            market("jupiter", "b", "Marco Rubio", event, "0.30"),
            market("jupiter", "c", "Tucker Carlson", event, "0.10"),
            panta,
        ];
        let out = build(&markets);
        assert_eq!(out.graph.events.len(), 1, "Panta's event folds into Jupiter's");
        let event = &out.graph.events[0];
        assert_eq!(event.venue_count, 2);
        assert_eq!(event.sources.len(), 2);
        let rubio = event.questions[0].outcomes.iter().find(|o| o.label == "Marco Rubio").unwrap();
        let venues: Vec<&str> = rubio.listings.iter().map(|l| l.venue_id.as_str()).collect();
        assert!(venues.contains(&"panta") && venues.contains(&"jupiter"), "{venues:?}");
        assert!(event.questions[0].rules_differ, "a scored merge is shown side by side, not ranked as identical");
        // The Panta event's own id still resolves.
        assert!(out.graph.event(&event_id("panta", "p")).is_some());
    }

    #[test]
    fn identical_price_questions_merge_into_one_question_with_both_venues() {
        let jupiter = market("jupiter", "j", "↑ 90,000", Some(("e1", "What price will Bitcoin hit by December 31, 2026?")), "0.40");
        let mut panta = market("panta", "p", "Will Bitcoin reach $90,000 by December 31, 2026?", None, "0.38");
        panta.event = Some(VenueEvent { event_id: "p".into(), title: panta.title.clone(), ..Default::default() });
        let out = build(&[jupiter, panta]);
        assert_eq!(out.graph.events.len(), 1);
        let q = &out.graph.events[0].questions[0];
        let yes = &q.outcomes[0];
        assert_eq!(yes.listings.len(), 2);
        assert_eq!(yes.listings[0].venue_id, "panta", "cheaper YES rates first");
        assert!(!q.rules_differ, "an exact structured match");
    }

    #[test]
    fn up_or_down_reads_as_a_question() {
        assert_eq!(up_down_sentence("Bitcoin Up or Down - September 24, 2:30PM-2:35PM ET"), "Will Bitcoin go up or down, September 24, 2:30PM-2:35PM ET?");
        assert_eq!(up_down_sentence("SPY Opens Up or Down on September 24?"), "Will SPY open up or down on September 24?");
        assert_eq!(up_down_sentence("Solana Up or Down"), "Will Solana go up or down?");
    }

    #[test]
    fn question_text_reads_as_a_sentence() {
        let blank = market("jupiter", "a", "82,000", Some(("e1", "Bitcoin above ___ on September 24?")), "0.4");
        let date = market("jupiter", "b", "December 31", Some(("e2", "Russia x Ukraine ceasefire agreement by...?")), "0.2");
        let out = build(&[blank, date]);
        let texts: Vec<&str> = out.graph.events.iter().flat_map(|e| e.questions.iter().map(|q| q.text.as_str())).collect();
        assert!(texts.contains(&"Bitcoin above 82,000 on September 24?"), "{texts:?}");
        assert!(texts.iter().any(|t| t.starts_with("Russia x Ukraine ceasefire agreement by Dec 31, ")), "{texts:?}");
    }

    #[test]
    fn ids_are_stable_across_rebuilds() {
        let event = Some(("e1", "What price will Bitcoin hit by December 31, 2026?"));
        let markets = [market("jupiter", "a", "↑ 90,000", event, "0.4")];
        let first = build(&markets);
        let second = build(&markets);
        assert_eq!(first.graph.events[0].id, second.graph.events[0].id);
        assert_eq!(first.graph.events[0].questions[0].id, second.graph.events[0].questions[0].id);
    }

    /// Builds the graph from a dump of real markets (a JSON array of `VenueMarket`) and prints
    /// what the events look like: `GRAPH_MARKETS=/path/markets.json cargo test -p metis-aggregator
    /// graph_report -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a market dump in GRAPH_MARKETS"]
    fn graph_report() {
        let path = std::env::var("GRAPH_MARKETS").expect("GRAPH_MARKETS");
        let markets: Vec<VenueMarket> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let refs: Vec<&VenueMarket> = markets.iter().collect();
        let now = chrono::Utc::now().timestamp();
        let graph = build_graph(&refs, &BuildConfig::default(), |_| true, now).graph;
        let grouped: Vec<&EventNode> = graph.events.iter().filter(|e| e.sources.len() > 1).collect();
        println!("markets {} -> events {} ({} grouped from {} venue events)", markets.len(), graph.events.len(), grouped.len(), grouped.iter().map(|e| e.sources.len()).sum::<usize>());
        let questionish = graph.events.iter().filter(|e| e.title.ends_with('?') || e.title.to_lowercase().starts_with("will ")).count();
        println!("titles still reading as a question: {questionish}");
        let mut by_size = grouped.clone();
        by_size.sort_by_key(|e| std::cmp::Reverse(e.sources.len()));
        println!("\n== largest groups");
        for e in by_size.iter().take(25) {
            println!("  [{}] {}\n      {}\n      e.g. {:?}", e.sources.len(), e.title, e.context, e.questions.iter().take(2).map(|q| q.text.as_str()).collect::<Vec<_>>());
        }
        println!("\n== sample of single events: venue title -> headline");
        for e in graph.events.iter().filter(|e| e.sources.len() == 1).step_by(40).take(60) {
            println!("  {:70} -> {}", e.sources[0].title.chars().take(70).collect::<String>(), e.title);
        }
    }
}
