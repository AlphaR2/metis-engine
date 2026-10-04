//! Builds the event graph from the venues' raw markets: profile → cluster → assemble.
//!
//! Pure: the same markets, hints and clock always build the same graph. An event is assembled from
//! its cluster's markets; the markets themselves are carried through **exactly as the venue sent
//! them**, annotated with a rating and the identical questions other venues list.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rust_decimal::Decimal;

use super::ai::{AiCluster, AiMarket, Cached, content_hash};
use super::cluster::{Cluster, Hint, Reason, ai_requests, cluster};
use super::dictionary::{Dictionary, EntityRef};
use super::extract;
use super::graph::{EventGraph, EventNode, EventStatus, Grouping, MarketNode, MarketRef, SourceRef, stable_id};
use super::headline;
use super::rating::{rate_event, rate_market, volume_of};
use super::subject::{Profile, profile, subject_terms};
use super::text;
use crate::models::venue::VenueMarket;

/// The markets of one build with their profiles, so the model-assisted step can look at the rules'
/// clusters before the graph is assembled.
pub struct Prepared<'a> {
    pub markets: Vec<&'a VenueMarket>,
    pub profiles: Vec<Profile>,
}

/// Profiles every bettable market. Markets are ordered by identity so nothing depends on the order
/// the store happened to hold them in.
pub fn prepare<'a>(markets: &[&'a VenueMarket], now: i64) -> Prepared<'a> {
    let mut markets: Vec<&VenueMarket> = markets.iter().copied().filter(|m| m.is_bettable(now)).collect();
    markets.sort_by(|a, b| (&a.venue_id, &a.venue_market_id).cmp(&(&b.venue_id, &b.venue_market_id)));
    let mut dictionary = Dictionary::seeded();
    extract::learn_from(markets.iter().copied(), &mut dictionary);
    let profiles = markets.iter().map(|m| profile(m, &dictionary)).collect();
    Prepared { markets, profiles }
}

impl Prepared<'_> {
    /// The clusters with a model's cached decisions applied on top of the rules. A decision is used
    /// only while the market's text is what it was made from.
    pub fn clusters_with(&self, cached: &HashMap<String, Cached>) -> Vec<Cluster> {
        let hints: HashMap<String, Hint> = self
            .markets
            .iter()
            .filter_map(|m| {
                let key = format!("{}:{}", m.venue_id, m.venue_market_id);
                cached.get(&key).filter(|c| c.content_hash == content_hash(m)).map(|c| (key, c.to_hint()))
            })
            .collect();
        cluster(&self.profiles, &hints)
    }

    /// The event graph, with a model's `hints` applied on top of the rules.
    #[cfg(test)]
    pub fn build(&self, hints: &HashMap<String, Hint>, routable: impl Fn(&str) -> bool, now: i64) -> EventGraph {
        self.build_with(cluster(&self.profiles, hints), routable, now)
    }

    /// The event graph for clusters already computed.
    pub fn build_with(&self, clusters: Vec<Cluster>, routable: impl Fn(&str) -> bool, now: i64) -> EventGraph {
        let max_volume = clusters
            .iter()
            .map(|c| c.members.iter().map(|&m| self.profiles[m].volume).sum::<Decimal>())
            .max()
            .unwrap_or(Decimal::ZERO);
        let mut events: Vec<EventNode> = clusters.iter().map(|c| self.assemble(c, max_volume, &routable, now)).collect();
        events.sort_by(|a, b| b.volume_usdc.cmp(&a.volume_usdc).then(a.close_time.cmp(&b.close_time)).then(a.id.cmp(&b.id)));
        EventGraph::new(events, now)
    }

    /// What to ask a model: the lone markets that resemble something and have no usable cached
    /// answer (at most `limit`, most-traded first), with the clusters they resemble.
    pub fn ai_input(&self, clusters: &[Cluster], cached: &HashMap<String, Cached>, limit: usize) -> AiInput {
        let keys: HashSet<&str> = clusters.iter().map(|c| c.key.as_str()).collect();
        let mut markets = Vec::new();
        let mut wanted: BTreeMap<usize, ()> = BTreeMap::new();
        let mut hashes = HashMap::new();
        for request in ai_requests(&self.profiles, clusters) {
            if markets.len() >= limit {
                break;
            }
            let market = self.markets[request.market];
            let key = self.profiles[request.market].key.clone();
            let hash = content_hash(market);
            let usable = cached.get(&key).is_some_and(|c| c.content_hash == hash && c.join.as_deref().is_none_or(|j| keys.contains(j)));
            if usable {
                continue;
            }
            for &c in &request.clusters {
                wanted.insert(c, ());
            }
            hashes.insert(key.clone(), hash);
            markets.push(AiMarket {
                id: key,
                title: market.title.clone(),
                event_title: market.event.as_ref().map(|e| e.title.clone()).filter(|t| !t.trim().is_empty()),
                category: market.category.as_str().to_string(),
                closes: text::date_of(market.close_time).map(|d| d.to_string()),
                candidates: request.clusters.iter().map(|&c| clusters[c].key.clone()).collect(),
            });
        }
        let clusters_out = wanted
            .keys()
            .map(|&c| {
                let cluster = &clusters[c];
                let members: Vec<&VenueMarket> = cluster.members.iter().map(|&m| self.markets[m]).collect();
                let profiles: Vec<&Profile> = cluster.members.iter().map(|&m| &self.profiles[m]).collect();
                AiCluster {
                    key: cluster.key.clone(),
                    title: event_title(cluster, &members, &profiles),
                    samples: members.iter().take(3).map(|m| m.title.clone()).collect(),
                }
            })
            .collect();
        AiInput { markets, clusters: clusters_out, hashes }
    }

    fn assemble(&self, cluster: &Cluster, max_volume: Decimal, routable: &impl Fn(&str) -> bool, now: i64) -> EventNode {
        let members: Vec<&VenueMarket> = cluster.members.iter().map(|&m| self.markets[m]).collect();
        let profiles: Vec<&Profile> = cluster.members.iter().map(|&m| &self.profiles[m]).collect();
        let leader = members[0];
        let leader_profile = profiles[0];
        let venue_event = leader.event.clone().unwrap_or_default();

        let title = event_title(cluster, &members, &profiles);
        let max_in_event = members.iter().map(|m| volume_of(m)).max().unwrap_or(Decimal::ZERO);
        let mut nodes: Vec<MarketNode> = members
            .iter()
            .map(|m| MarketNode {
                market: (*m).clone(),
                rating: rate_market(m, max_in_event, routable(&m.venue_id)),
                same_question_as: Vec::new(),
            })
            .collect();
        link_identical_questions(&mut nodes);
        // Markets another venue also lists lead: seeing the same question priced on two venues is
        // what the aggregator is for, so they come before anything merely well-rated.
        nodes.sort_by(|a, b| {
            b.is_matched()
                .cmp(&a.is_matched())
                .then(b.rating.score.cmp(&a.rating.score)).then(b.volume().cmp(&a.volume())).then(a.market.venue_market_id.cmp(&b.market.venue_market_id))
        });

        let venues: Vec<String> = members.iter().map(|m| m.venue_id.clone()).collect::<BTreeSet<_>>().into_iter().collect();
        let volume: Decimal = members.iter().map(|m| volume_of(m)).sum();
        let close_time = members.iter().map(|m| m.close_time).filter(|t| *t > 0).min().unwrap_or(0);
        let start_time = members.iter().filter_map(|m| m.event.as_ref().and_then(|e| e.start_time)).filter(|t| *t > 0).min();
        let status = match start_time {
            _ if close_time > 0 && close_time <= now => EventStatus::Closed,
            Some(start) if start <= now => EventStatus::Live,
            _ => EventStatus::Upcoming,
        };

        let mut categories: BTreeMap<&str, usize> = BTreeMap::new();
        for m in &members {
            *categories.entry(m.category.as_str()).or_default() += 1;
        }
        let category = categories
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map_or_else(|| leader.category.as_str().to_string(), |(c, _)| (*c).to_string());

        let mut tags: Vec<String> = Vec::new();
        for m in &members {
            if let Some(event) = &m.event {
                for tag in event.tags.iter().chain(event.league.iter()).chain(event.subcategory.iter()) {
                    if tags.len() < 12 && !tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                        tags.push(tag.clone());
                    }
                }
            }
        }
        let mut entities: Vec<EntityRef> = Vec::new();
        for p in &profiles {
            for e in &p.entities {
                if entities.len() < 10 && !entities.contains(e) {
                    entities.push(e.clone());
                }
            }
        }

        let mut sources: Vec<SourceRef> = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for m in &members {
            if let Some(event) = &m.event
                && seen.insert((m.venue_id.clone(), event.event_id.clone()))
            {
                sources.push(SourceRef {
                    venue_id: m.venue_id.clone(),
                    venue_event_id: event.event_id.clone(),
                    title: event.title.trim().to_string(),
                    venue_category: m.venue_category.clone(),
                });
            }
        }

        let description = members.iter().find_map(|m| m.event.as_ref().and_then(|e| e.description.clone()).filter(|d| !d.trim().is_empty())).or_else(|| {
            leader.rules.as_ref().and_then(|r| r.split(['.', '\n']).map(str::trim).find(|s| s.len() > 20)).map(|s| format!("{s}."))
        });

        let domain = leader_profile.domain.as_str().to_string();
        EventNode {
            id: stable_id("evt", &cluster.key),
            slug: venue_event.slug.clone().filter(|_| cluster.members.len() == 1).unwrap_or_else(|| text::slug(&title)),
            title,
            subtitle: (cluster.members.len() == 1).then(|| venue_event.subtitle.clone()).flatten(),
            description,
            category,
            subcategory: members.iter().find_map(|m| m.event.as_ref().and_then(|e| e.league.clone().or_else(|| e.subcategory.clone()))),
            domain,
            tags,
            entities,
            image_url: members.iter().find_map(|m| m.event.as_ref().and_then(|e| e.image_url.clone()).filter(|u| !u.trim().is_empty())),
            start_time,
            close_time,
            status,
            volume_usdc: volume,
            venue_count: venues.len(),
            market_count: nodes.len(),
            rating: rate_event(volume, max_volume, venues.len(), nodes.len()),
            venues,
            grouping: Grouping { reason: cluster.reason.as_str().to_string(), detail: cluster.detail.clone(), confidence: cluster.confidence },
            sources,
            markets: nodes,
        }
    }
}

/// What a model is asked about in one refresh.
#[derive(Debug, Default)]
pub struct AiInput {
    pub markets: Vec<AiMarket>,
    pub clusters: Vec<AiCluster>,
    /// Market key → the hash of the text the answer will be cached against.
    pub hashes: HashMap<String, String>,
}

/// Builds the whole graph with the rules alone.
#[cfg(test)]
pub fn build_events(markets: &[&VenueMarket], routable: impl Fn(&str) -> bool, now: i64) -> EventGraph {
    prepare(markets, now).build(&HashMap::new(), routable, now)
}

// ── Titles ──────────────────────────────────────────────────────────────────────────────────

/// A headline for the event: never a question, and never a rewrite of a market.
fn event_title(cluster: &Cluster, members: &[&VenueMarket], profiles: &[&Profile]) -> String {
    if let Some(title) = cluster.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        return title.to_string();
    }
    let leader = members[0];
    let own_title = |m: &VenueMarket| -> String {
        let raw = m.event.as_ref().map(|e| e.title.trim()).filter(|t| !t.is_empty()).unwrap_or(m.title.trim());
        let head = headline::headline(raw);
        if head.split_whitespace().count() >= 2 { head } else { raw.to_string() }
    };
    match cluster.reason {
        Reason::Match => {
            let p = profiles[0];
            if let [a, b] = p.participants.as_slice() {
                return format!("{} vs {}", a.name, b.name);
            }
        }
        Reason::Price => {
            if let (Some(subject), Some(day)) = (&profiles[0].subject, profiles[0].target_date.or(profiles[0].close_date)) {
                return format!("{} · {}", subject.name, day.format("%b %-d, %Y"));
            }
        }
        Reason::Terms | Reason::Ai if members.len() > 1 => {
            let shared: BTreeSet<String> = profiles
                .iter()
                .map(|p| p.terms.clone())
                .reduce(|a, b| a.intersection(&b).cloned().collect())
                .unwrap_or_default();
            if shared.len() >= 2 {
                let source = leader.event.as_ref().map_or(leader.title.as_str(), |e| e.title.as_str());
                if let Some(title) = shared_title(source, &shared).or_else(|| shared_title(&leader.title, &shared)) {
                    return title;
                }
            }
        }
        _ => {}
    }
    let _ = subject_terms; // kept for the title fallbacks above
    own_title(leader)
}

/// The words of `text` that the whole cluster shares, in the text's own order and casing, with a
/// season number kept next to "season": "A female housemate to win Big Brother Naija Season 11?"
/// with {big, brother, naija, season} gives "Big Brother Naija Season 11".
fn shared_title(source: &str, shared: &BTreeSet<String>) -> Option<String> {
    let mut kept: Vec<&str> = Vec::new();
    let mut previous_kept = false;
    for token in source.split_whitespace() {
        let word = token.trim_matches(|c: char| !c.is_alphanumeric());
        if word.is_empty() {
            previous_kept = false;
            continue;
        }
        let folded = text::fold(word);
        let numeric = word.chars().all(|c| c.is_ascii_digit());
        let keep = shared.contains(&folded) || (numeric && previous_kept && kept.last().is_some_and(|w| text::fold(w) == "season"));
        if keep {
            kept.push(word);
        }
        previous_kept = keep;
    }
    // Two words ("launch token") name nothing; the leader's own headline does better.
    (kept.len() >= 3).then(|| kept.join(" "))
}

// ── Identical questions ─────────────────────────────────────────────────────────────────────

/// Tags markets that ask the same question on different venues, so they can sit side by side. Two
/// markets are the same question when their distinctive words and their numbers agree.
fn link_identical_questions(nodes: &mut [MarketNode]) {
    let signatures: Vec<QuestionSignature> = nodes.iter().map(|n| QuestionSignature::of(&n.market)).collect();
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            if nodes[i].market.venue_id == nodes[j].market.venue_id {
                continue;
            }
            if signatures[i].same_question(&signatures[j]) {
                let (ri, rj) = (ref_of(&nodes[i].market), ref_of(&nodes[j].market));
                nodes[i].same_question_as.push(rj);
                nodes[j].same_question_as.push(ri);
            }
        }
    }
}

/// Which way a market bets on a threshold: "hit $100,000", "↑ 100,000" and "above 100k" all bet
/// up; "↓ 45,000", "dip to" and "below" bet down. Two markets on the same number that bet opposite
/// ways are different questions, so the direction has to agree whenever both state one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Up,
    Down,
}

impl Direction {
    fn of(text: &str) -> Option<Self> {
        let words: Vec<String> = text.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase).collect();
        let has = |set: &[&str]| words.iter().any(|w| set.contains(&w.as_str()));
        let up = text.contains('↑') || has(&["hit", "hits", "reach", "reaches", "above", "over", "exceed", "exceeds", "surpass", "higher", "rise"]);
        let down = text.contains('↓') || has(&["dip", "dips", "below", "under", "fall", "falls", "drop", "drops", "lower"]);
        match (up, down) {
            (true, false) => Some(Self::Up),
            (false, true) => Some(Self::Down),
            _ => None,
        }
    }
}

/// What a market asks, read from its whole question - the event's title plus its own - not just its
/// own title. Jupiter lists a threshold market as the bare "↑ 100,000" under an event called
/// "Bitcoin · Dec 31, 2026", so its own title alone says nothing of what it is about.
struct QuestionSignature {
    terms: BTreeSet<String>,
    numbers: Vec<Decimal>,
    direction: Option<Direction>,
}

/// Words that say when or which way, not what: the date and direction are compared as numbers and
/// `Direction`, so leaving them out of the words stops "31 Dec" vs "December 31" from counting as a
/// difference.
const WHEN_WORDS: &[&str] = &[
    "jan", "january", "feb", "february", "mar", "march", "apr", "april", "may", "jun", "june", "jul", "july", "aug", "august", "sep", "sept",
    "september", "oct", "october", "nov", "november", "dec", "december", "by", "on", "in", "the",
];

impl QuestionSignature {
    fn of(market: &VenueMarket) -> Self {
        let full = market.compose_display_title();
        let mut numbers = text::numbers(&full);
        numbers.sort();
        numbers.dedup();
        let terms = subject_terms(&full).into_iter().filter(|t| !WHEN_WORDS.contains(&t.as_str())).collect();
        Self { terms, numbers, direction: Direction::of(&full) }
    }

    /// Same distinctive words (80% overlap), the same numbers, and no opposite directions.
    fn same_question(&self, other: &Self) -> bool {
        if self.numbers != other.numbers || self.terms.is_empty() || other.terms.is_empty() {
            return false;
        }
        if let (Some(a), Some(b)) = (self.direction, other.direction)
            && a != b
        {
            return false;
        }
        let shared = self.terms.intersection(&other.terms).count() as f64;
        let union = self.terms.union(&other.terms).count() as f64;
        shared / union >= 0.8
    }
}

fn ref_of(market: &VenueMarket) -> MarketRef {
    MarketRef { venue_id: market.venue_id.clone(), venue_market_id: market.venue_market_id.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::venue::{Category, VenueEvent};

    const NOW: i64 = 1_789_000_000;

    fn market(venue: &str, id: &str, title: &str, event: Option<VenueEvent>, volume: u32) -> VenueMarket {
        VenueMarket {
            venue_id: venue.into(),
            venue_market_id: id.into(),
            title: title.into(),
            event,
            category: Category::from("culture"),
            venue_category: "culture".into(),
            close_time: NOW + 2 * 86_400,
            resolution_time: NOW + 3 * 86_400,
            yes_price: Some("0.52".into()),
            no_price: Some("0.48".into()),
            liquidity: Some(Decimal::from(volume)),
            ..Default::default()
        }
    }

    fn filler(n: usize) -> Vec<VenueMarket> {
        (0..n)
            .map(|i| market("panta", &format!("f{i}"), &format!("Will filler{i} subject{i} happen on topic{i}?"), None, 1))
            .collect()
    }

    fn build(markets: &[VenueMarket]) -> EventGraph {
        let refs: Vec<&VenueMarket> = markets.iter().collect();
        build_events(&refs, |_| true, NOW)
    }

    #[test]
    fn the_bbnaija_markets_form_one_event_that_holds_both_untouched() {
        let image = VenueEvent { event_id: "e".into(), title: "BB".into(), image_url: Some("https://img.example/bb.png".into()), ..Default::default() };
        let mut markets = vec![
            market("panta", "female", "A female housemate to win Big Brother Naija Season 11?", Some(image), 491),
            market("panta", "tram", "Will Tram finish in the Top 3 of BBNaija Season 11?", None, 0),
        ];
        markets.extend(filler(30));
        let graph = build(&markets);
        let event = graph.events.iter().find(|e| e.markets.iter().any(|m| m.market.venue_market_id == "tram")).unwrap();

        assert_eq!(event.title, "Big Brother Naija Season 11");
        assert_eq!(event.market_count, 2);
        assert_eq!(event.grouping.reason, "terms");
        assert_eq!(event.image_url.as_deref(), Some("https://img.example/bb.png"));
        assert_eq!(event.volume_usdc, Decimal::from(491));
        // The markets are the venue's own, byte for byte.
        for node in &event.markets {
            let original = markets.iter().find(|m| m.venue_market_id == node.market.venue_market_id).unwrap();
            assert_eq!(serde_json::to_value(&node.market).unwrap(), serde_json::to_value(original).unwrap());
        }
    }

    /// The six Panta markets from the first live run, titles exactly as the venue sent them (note the
    /// invisible word joiner some carry after the question mark).
    #[test]
    fn the_first_live_panta_markets_group_the_way_the_user_expects() {
        let titles = [
            ("a", "A female housemate to win Big Brother Naija Season 11?\u{2060}"),
            ("b", "Manchester United will win against Spurs with 2 or more goals"),
            ("c", "The target in the $JUMP token sale to exceed $35m by the october 2 deadline"),
            ("d", "Tram to finish in the Top 3 of BBNaija Season 11?\u{2060}"),
            ("e", "GTA 6 to release on November 19th, 2026"),
            ("f", "Bitcoin to hit $100,000 by 31 Dec 2026"),
        ];
        let markets: Vec<VenueMarket> = titles.iter().map(|(id, t)| market("panta", id, t, None, 10)).collect();
        let graph = build(&markets);
        assert_eq!(graph.events.len(), 5, "the two BBNaija markets are one event: {:?}", graph.events.iter().map(|e| &e.title).collect::<Vec<_>>());
        let bb = graph.events.iter().find(|e| e.market_count == 2).unwrap();
        let ids: Vec<&str> = bb.markets.iter().map(|m| m.market.venue_market_id.as_str()).collect();
        assert!(ids.contains(&"a") && ids.contains(&"d"));
        assert!(bb.title.contains("Naija"), "{}", bb.title);
    }

    #[test]
    fn a_market_that_matches_nothing_is_its_own_event_with_the_venues_metadata() {
        let event = VenueEvent { event_id: "E9".into(), title: "Will GTA 6 release on November 19th, 2026?".into(), image_url: Some("https://img.example/gta.png".into()), ..Default::default() };
        let mut markets = vec![market("panta", "gta", "Will GTA 6 release on November 19th, 2026?", Some(event), 5)];
        markets.extend(filler(20));
        let graph = build(&markets);
        let gta = graph.events.iter().find(|e| e.markets[0].market.venue_market_id == "gta").unwrap();
        assert_eq!(gta.market_count, 1);
        assert_eq!(gta.grouping.reason, "single", "nothing else matched, so it is its own event");
        assert_eq!(gta.image_url.as_deref(), Some("https://img.example/gta.png"));
        assert!(!gta.title.ends_with('?'), "an event title is a headline: {}", gta.title);
    }

    #[test]
    fn the_same_question_on_two_venues_is_tagged_not_merged() {
        let mut markets = vec![
            market("panta", "p", "Will Quincy Orbit launch before December 2026?", None, 10),
            market("jupiter", "j", "Will Quincy Orbit launch before December 2026?", None, 30),
        ];
        markets.extend(filler(20));
        let graph = build(&markets);
        let event = graph.events.iter().find(|e| e.market_count == 2).expect("one event, two markets");
        assert_eq!(event.venue_count, 2);
        for node in &event.markets {
            assert_eq!(node.same_question_as.len(), 1);
        }
        let p = event.markets.iter().find(|n| n.market.venue_id == "panta").unwrap();
        assert_eq!(p.same_question_as[0].venue_market_id, "j");
    }

    #[test]
    fn markets_inside_an_event_are_sorted_by_our_rating() {
        let mut markets = vec![
            market("panta", "quiet", "Will Quincy Orbit launch before December 2026?", None, 1),
            market("panta", "busy", "Will Quincy Orbit launch in November 2026?", None, 900),
        ];
        markets.extend(filler(20));
        let graph = build(&markets);
        let event = graph.events.iter().find(|e| e.market_count == 2).unwrap();
        assert_eq!(event.markets[0].market.venue_market_id, "busy");
        assert!(event.markets[0].rating.score >= event.markets[1].rating.score);
    }

    #[test]
    fn building_is_deterministic_whatever_the_input_order() {
        let mut markets = vec![
            market("panta", "female", "A female housemate to win Big Brother Naija Season 11?", None, 491),
            market("panta", "tram", "Will Tram finish in the Top 3 of BBNaija Season 11?", None, 0),
        ];
        markets.extend(filler(15));
        let a = build(&markets);
        markets.reverse();
        let b = build(&markets);
        let ids = |g: &EventGraph| g.events.iter().map(|e| (e.id.clone(), e.market_count)).collect::<Vec<_>>();
        assert_eq!(ids(&a), ids(&b));
    }

    #[test]
    fn the_graph_finds_a_market_and_its_event() {
        let mut markets = vec![market("panta", "female", "A female housemate to win Big Brother Naija Season 11?", None, 491)];
        markets.extend(filler(3));
        let graph = build(&markets);
        let (event, node) = graph.find_market("panta", "female").unwrap();
        assert_eq!(node.market.venue_market_id, "female");
        assert!(graph.event(&event.id).is_some());
    }

    #[test]
    fn a_threshold_market_matches_across_venues_through_its_event_title() {
        let jup = |id: &str, title: &str| {
            let mut m = market("jupiter", id, title, None, 5);
            m.event = Some(VenueEvent { event_id: "e".into(), title: "Bitcoin · Dec 31, 2026".into(), ..Default::default() });
            m
        };
        let panta = market("panta", "p", "Will bitcoin hit $100,000 by 31 Dec 2026", None, 1);
        let up = jup("up", "↑ 100,000");
        let down = jup("down", "↓ 100,000");
        let other = jup("other", "↑ 250,000");
        let sig = QuestionSignature::of;
        assert!(sig(&panta).same_question(&sig(&up)), "hit $100,000 is the same bet as ↑ 100,000");
        assert!(!sig(&panta).same_question(&sig(&down)), "the opposite direction is a different question");
        assert!(!sig(&panta).same_question(&sig(&other)), "a different threshold is a different question");
    }

    #[test]
    fn a_question_listed_on_two_venues_leads_its_event_however_quiet_it_is() {
        let mut markets = vec![
            market("jupiter", "loud", "Will Quincy Orbit exceed 500 launches before December 2026?", None, 9_000),
            market("panta", "p", "Will Quincy Orbit launch before December 2026?", None, 1),
            market("jupiter", "j", "Will Quincy Orbit launch before December 2026?", None, 2),
        ];
        markets.extend(filler(20));
        let graph = build(&markets);
        let event = graph.events.iter().find(|e| e.market_count == 3).expect("one event, three markets");
        assert_eq!(event.matched_markets(), 2);
        assert!(event.markets[0].is_matched() && event.markets[1].is_matched(), "the matched pair leads, ahead of the 9000-volume market");
        assert_eq!(event.markets[2].market.venue_market_id, "loud");
    }

    /// Runs the matcher over the saved venue captures in `fixtures/` and prints what pairs across
    /// venues. A look at real data, not an assertion: the captures are a snapshot, so what matches
    /// changes with them. Run with `cargo test -p metis-aggregator --bin metis-api fixture_pairs -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads the saved captures and prints; run on demand"]
    fn fixture_pairs_across_venues() {
        use crate::models::venue::jupiter::JupEventsResponse;
        use crate::models::venue::panta::PantaMarketRow;
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let mut markets: Vec<VenueMarket> = Vec::new();
        for entry in std::fs::read_dir(dir.join("jupiter")).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("events-") || name == "events-all.json" {
                continue;
            }
            let Ok(parsed) = serde_json::from_slice::<JupEventsResponse>(&std::fs::read(entry.path()).unwrap()) else { continue };
            for event in &parsed.data {
                for m in &event.markets {
                    let mut vm = crate::services::jupiter::client::market_to_venue_market(Some(event), m);
                    vm.venue_id = "jupiter".into();
                    markets.push(vm);
                }
            }
        }
        let jupiter = markets.len();
        for entry in std::fs::read_dir(dir.join("panta")).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("market-") {
                continue;
            }
            if let Ok(row) = serde_json::from_slice::<PantaMarketRow>(&std::fs::read(entry.path()).unwrap()) {
                let mut vm = VenueMarket::from(row);
                vm.venue_id = "panta".into();
                // The captured rows are all resolved or cancelled, so none would be built. One
                // still closes in the future; treat it as live so the matcher sees a real pair.
                if vm.close_time > 1_790_248_525 {
                    vm.dead = false;
                }
                markets.push(vm);
            }
        }
        println!("loaded {jupiter} jupiter + {} panta markets", markets.len() - jupiter);
        let refs: Vec<&VenueMarket> = markets.iter().collect();
        let graph = build_events(&refs, |_| true, 1_790_248_525);
        let multi: Vec<_> = graph.events.iter().filter(|e| e.venue_count > 1).collect();
        println!("{} events, {} on several venues", graph.events.len(), multi.len());
        for e in multi {
            println!("\n[{}] {} ({} markets, {} matched)", e.grouping.reason, e.title, e.market_count, e.matched_markets());
            for n in e.markets.iter().filter(|n| n.is_matched() || n.market.venue_id == "panta").take(6) {
                println!("   {:>7} {} | {}", n.market.venue_id, n.market.display_title.chars().take(70).collect::<String>(), if n.is_matched() { "MATCHED" } else { "" });
            }
        }
    }
}
