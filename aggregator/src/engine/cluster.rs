//! Groups markets into events: the same real-world subject, whichever venue lists it and however
//! many questions it raises.
//!
//! Rules first, each recorded as the *reason* for the grouping:
//!
//! 1. **Seeds.** Markets that already share something exact are joined: the same venue event
//!    (`venue_event`), the same two sides on the same day (`match`), the same asset on the same day
//!    (`price`). Same-venue markets are clustered exactly like cross-venue ones.
//! 2. **Terms.** What is left is grouped by distinctive words: a unit joins the cluster whose words
//!    cover most of its own rare words, unless a hard conflict applies (a different season, year,
//!    kind of subject, or betting that closes far apart). This is what puts "Tram to finish in the
//!    Top 3 of BBNaija Season 11" and "A female housemate to win Big Brother Naija Season 11" in one
//!    event.
//! 3. **Hints.** Whatever the rules left alone can be assigned by a model (`ai`); a hint is checked
//!    against the same conflicts before it is applied.
//!
//! Pure and deterministic: the same profiles and hints always give the same clusters.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rust_decimal::Decimal;

use super::extract::Domain;
use super::subject::{Profile, conflicts};

/// The most markets an event may hold when *we* group them. A venue's own event is trusted up to
/// `VENUE_EVENT_CAP`: some (the largest company by market cap) really do list hundreds of options.
pub const MAX_MEMBERS: usize = 60;
pub const VENUE_EVENT_CAP: usize = 300;

/// A term in more than this share of all markets says nothing about a subject.
const GENERIC_SHARE: f64 = 0.05;

/// The share of a unit's own rare words a cluster must cover for the unit to join it.
const MIN_COVERAGE: f64 = 0.6;

/// The fewest shared rare words that can join two units.
const MIN_SHARED_TERMS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    VenueEvent,
    Match,
    Price,
    Terms,
    Ai,
    Single,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::VenueEvent => "venue_event",
            Reason::Match => "match",
            Reason::Price => "price",
            Reason::Terms => "terms",
            Reason::Ai => "ai",
            Reason::Single => "single",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Cluster {
    /// A stable reference to this cluster, also what a model hint names.
    pub key: String,
    /// Indexes into the profiles slice, most-traded first.
    pub members: Vec<usize>,
    pub reason: Reason,
    pub detail: String,
    pub confidence: f32,
    /// A title a model gave, when it assigned this cluster.
    pub title: Option<String>,
}

/// What a model decided for one market.
#[derive(Debug, Clone, Default)]
pub struct Hint {
    /// Join the existing cluster with this key.
    pub join: Option<String>,
    /// Or join the other markets that share this group label, as a new event.
    pub group: Option<String>,
    /// A headline for the event, when the model wrote one.
    pub title: Option<String>,
}

// ── Union-find over market indexes ──────────────────────────────────────────────────────────

struct Dsu {
    parent: Vec<usize>,
}

impl Dsu {
    fn new(n: usize) -> Self {
        Self { parent: (0..n).collect() }
    }
    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut cur = x;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            // The smaller index is the root, so results do not depend on call order.
            self.parent[ra.max(rb)] = ra.min(rb);
        }
    }
}

/// The exact keys a market is seeded under, strongest first.
fn seed_keys(p: &Profile) -> Vec<(Reason, String)> {
    let mut keys = Vec::new();
    if let Some((venue, event)) = &p.venue_event {
        keys.push((Reason::VenueEvent, format!("ve:{venue}:{event}")));
    }
    let day = p.target_date.or(p.close_date);
    if p.participants.len() == 2
        && let Some(day) = day
    {
        keys.push((Reason::Match, format!("match:{}|{}:{day}", p.participants[0].key, p.participants[1].key)));
    }
    if let Some(subject) = &p.subject
        && matches!(p.domain, Domain::Crypto | Domain::Finance | Domain::Commodity)
        && let Some(day) = day
    {
        keys.push((Reason::Price, format!("px:{}:{day}", subject.key)));
    }
    keys
}

/// A cluster under construction.
struct Unit {
    members: Vec<usize>,
    reason: Reason,
    key: String,
    detail: String,
    confidence: f32,
    title: Option<String>,
}

impl Unit {
    fn volume(&self, profiles: &[Profile]) -> Decimal {
        self.members.iter().map(|&m| profiles[m].volume).sum()
    }
}

fn seed_units(profiles: &[Profile]) -> Vec<Unit> {
    let n = profiles.len();
    let mut dsu = Dsu::new(n);
    let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, p) in profiles.iter().enumerate() {
        for (_, key) in seed_keys(p) {
            by_key.entry(key).or_default().push(i);
        }
    }
    let mut keys: Vec<&String> = by_key.keys().collect();
    keys.sort();

    // 1. What a venue grouped itself, trusted up to a generous cap.
    for key in keys.iter().filter(|k| k.starts_with("ve:")) {
        let members = &by_key[*key];
        if members.len() <= VENUE_EVENT_CAP {
            for pair in members.windows(2) {
                dsu.union(pair[0], pair[1]);
            }
        }
    }
    // 2. What we can prove exactly (the same two sides, the same asset on the same day). These are
    // merged only while the result stays a readable event: joining one market of a venue event pulls
    // in the whole of it, so unchecked they chain unrelated events into one.
    let mut size: HashMap<usize, usize> = HashMap::new();
    for i in 0..n {
        *size.entry(dsu.find(i)).or_default() += 1;
    }
    for key in keys.iter().filter(|k| !k.starts_with("ve:")) {
        let members = &by_key[*key];
        let Some(&first) = members.first() else { continue };
        for &m in &members[1..] {
            let (r0, r) = (dsu.find(first), dsu.find(m));
            if r0 == r {
                continue;
            }
            let combined = size[&r0] + size[&r];
            if combined <= MAX_MEMBERS {
                dsu.union(r0, r);
                let root = dsu.find(r0);
                size.insert(root, combined);
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let root = dsu.find(i);
        groups.entry(root).or_default().push(i);
    }
    groups
        .into_values()
        .map(|mut members| {
            members.sort_by(|&a, &b| profiles[b].volume.cmp(&profiles[a].volume).then(profiles[a].key.cmp(&profiles[b].key)));
            // The strongest reason among the keys its members share.
            let mut best: Option<(Reason, String)> = None;
            for &m in &members {
                for (reason, key) in seed_keys(&profiles[m]) {
                    if by_key.get(&key).is_some_and(|v| v.len() > 1 && v.len() <= VENUE_EVENT_CAP) {
                        let rank = |r: Reason| match r {
                            Reason::Match => 0,
                            Reason::Price => 1,
                            Reason::VenueEvent => 2,
                            _ => 3,
                        };
                        if best.as_ref().is_none_or(|(r, _)| rank(reason) < rank(*r)) {
                            best = Some((reason, key));
                        }
                    }
                }
            }
            let (reason, key, detail, confidence) = match best {
                Some((reason, key)) => {
                    let detail = match reason {
                        Reason::VenueEvent => "The venue listed these under one event.".to_string(),
                        Reason::Match => "The same two sides on the same day.".to_string(),
                        _ => "The same asset on the same day.".to_string(),
                    };
                    let confidence = match reason {
                        Reason::VenueEvent => 0.95,
                        _ => 0.9,
                    };
                    (reason, key, detail, confidence)
                }
                None => (Reason::Single, format!("single:{}", profiles[members[0]].key), "No other market matched.".to_string(), 0.5),
            };
            Unit { members, reason, key, detail, confidence, title: None }
        })
        .collect()
}

// ── Terms ───────────────────────────────────────────────────────────────────────────────────

struct TermStats {
    idf: HashMap<String, f64>,
    /// Terms rare enough to identify a subject.
    rare: BTreeSet<String>,
}

fn term_stats(profiles: &[Profile]) -> TermStats {
    let n = profiles.len().max(1) as f64;
    let mut df: HashMap<&str, usize> = HashMap::new();
    for p in profiles {
        for t in &p.terms {
            *df.entry(t.as_str()).or_default() += 1;
        }
    }
    let mut idf = HashMap::new();
    let mut rare = BTreeSet::new();
    for (term, count) in df {
        idf.insert(term.to_string(), ((n + 1.0) / (count as f64 + 0.5)).ln());
        // A term is generic when it is in a notable share of markets. The floor keeps small
        // catalogs from calling everything generic.
        let generic_at = (n * GENERIC_SHARE).max(8.0);
        if (count as f64) <= generic_at {
            rare.insert(term.to_string());
        }
    }
    TermStats { idf, rare }
}

fn unit_terms(unit: &Unit, profiles: &[Profile], stats: &TermStats) -> BTreeSet<String> {
    unit.members.iter().flat_map(|&m| profiles[m].terms.iter()).filter(|t| stats.rare.contains(*t)).cloned().collect()
}

/// A cluster's identifying words: those its leader has, and those at least half its members share.
fn core_terms(unit: &Unit, profiles: &[Profile], stats: &TermStats) -> BTreeSet<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for &m in &unit.members {
        for t in &profiles[m].terms {
            if stats.rare.contains(t) {
                *counts.entry(t.as_str()).or_default() += 1;
            }
        }
    }
    let half = unit.members.len().div_ceil(2);
    let leader = &profiles[unit.members[0]].terms;
    counts.into_iter().filter(|(t, c)| *c >= half || leader.contains(*t)).map(|(t, _)| t.to_string()).collect()
}

fn mass(terms: impl IntoIterator<Item = impl AsRef<str>>, stats: &TermStats) -> f64 {
    terms.into_iter().map(|t| stats.idf.get(t.as_ref()).copied().unwrap_or(0.0)).sum()
}

/// Whether any market of `a` conflicts with the leader of `b`, or the reverse.
fn units_conflict(a: &Unit, b: &Unit, profiles: &[Profile]) -> bool {
    let (la, lb) = (&profiles[a.members[0]], &profiles[b.members[0]]);
    if conflicts(la, lb) {
        return true;
    }
    // Every member is checked against the other side's leader, so one dissimilar member cannot
    // ride in on a similar leader.
    a.members.iter().any(|&m| conflicts(&profiles[m], lb)) || b.members.iter().any(|&m| conflicts(la, &profiles[m]))
}

fn merge_by_terms(mut units: Vec<Unit>, profiles: &[Profile]) -> Vec<Unit> {
    let stats = term_stats(profiles);
    // Most-traded first: the busiest unit anchors a cluster and smaller ones join it.
    units.sort_by(|a, b| b.volume(profiles).cmp(&a.volume(profiles)).then(a.key.cmp(&b.key)));

    let mut accepted: Vec<Unit> = Vec::new();
    let mut cores: Vec<BTreeSet<String>> = Vec::new();
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();

    for unit in units {
        let own = unit_terms(&unit, profiles, &stats);
        let own_mass = mass(&own, &stats);
        // Candidate clusters: those sharing at least one rare word.
        let mut candidates: BTreeSet<usize> = BTreeSet::new();
        for term in &own {
            if let Some(ids) = index.get(term) {
                candidates.extend(ids);
            }
        }
        let mut best: Option<(usize, f64, Vec<String>)> = None;
        if own_mass > 0.0 {
            for c in candidates {
                if accepted[c].members.len() + unit.members.len() > MAX_MEMBERS {
                    continue;
                }
                let shared: Vec<String> = own.intersection(&cores[c]).cloned().collect();
                if shared.len() < MIN_SHARED_TERMS {
                    continue;
                }
                let coverage = mass(&shared, &stats) / own_mass;
                if coverage < MIN_COVERAGE || units_conflict(&unit, &accepted[c], profiles) {
                    continue;
                }
                if best.as_ref().is_none_or(|(_, s, _)| coverage > *s) {
                    best = Some((c, coverage, shared));
                }
            }
        }
        match best {
            Some((c, coverage, shared)) => {
                let host = &mut accepted[c];
                host.members.extend(unit.members);
                host.members.sort_by(|&a, &b| profiles[b].volume.cmp(&profiles[a].volume).then(profiles[a].key.cmp(&profiles[b].key)));
                // A cluster that started as a seed keeps its exact reason; a lone market that grew
                // by words is a `terms` cluster.
                if host.reason == Reason::Single {
                    host.reason = Reason::Terms;
                    host.key = format!("terms:{}", profiles[host.members[0]].key);
                    host.confidence = (0.6 + 0.3 * coverage as f32).min(0.9);
                }
                if host.reason == Reason::Terms {
                    host.detail = format!("They share: {}.", shared.join(", "));
                }
                cores[c] = core_terms(host, profiles, &stats);
                for term in &cores[c] {
                    let ids = index.entry(term.clone()).or_default();
                    if !ids.contains(&c) {
                        ids.push(c);
                    }
                }
            }
            None => {
                let id = accepted.len();
                let core = core_terms(&unit, profiles, &stats);
                for term in &core {
                    index.entry(term.clone()).or_default().push(id);
                }
                cores.push(core);
                accepted.push(unit);
            }
        }
    }
    accepted
}

// ── Public entry ────────────────────────────────────────────────────────────────────────────

/// Clusters the profiled markets. `hints` are a model's decisions for markets the rules left alone.
pub fn cluster(profiles: &[Profile], hints: &HashMap<String, Hint>) -> Vec<Cluster> {
    let mut units = merge_by_terms(seed_units(profiles), profiles);
    apply_hints(&mut units, profiles, hints);
    let mut clusters: Vec<Cluster> = units
        .into_iter()
        .map(|u| Cluster { key: u.key, members: u.members, reason: u.reason, detail: u.detail, confidence: u.confidence, title: u.title })
        .collect();
    clusters.sort_by(|a, b| a.key.cmp(&b.key));
    clusters
}

/// A lone market a model may be asked about, with the clusters it weakly resembles.
#[derive(Debug, Clone)]
pub struct AiRequest {
    /// Index into the profiles slice.
    pub market: usize,
    /// Indexes into the clusters slice: clusters sharing at least one rare word, best first.
    pub clusters: Vec<usize>,
}

/// The lone markets worth asking a model about: those that share at least one rare word with some
/// other cluster (a market resembling nothing cannot be helped), most-traded first.
pub fn ai_requests(profiles: &[Profile], clusters: &[Cluster]) -> Vec<AiRequest> {
    const MAX_CANDIDATES: usize = 6;
    let stats = term_stats(profiles);
    let mut index: HashMap<&str, Vec<usize>> = HashMap::new();
    for (c, cluster) in clusters.iter().enumerate() {
        let mut terms: BTreeSet<&str> = BTreeSet::new();
        for &m in &cluster.members {
            terms.extend(profiles[m].terms.iter().filter(|t| stats.rare.contains(*t)).map(String::as_str));
        }
        for term in terms {
            index.entry(term).or_default().push(c);
        }
    }
    let mut out: Vec<AiRequest> = Vec::new();
    for c in singletons(clusters) {
        let m = clusters[c].members[0];
        let mut scores: HashMap<usize, f64> = HashMap::new();
        for term in profiles[m].terms.iter().filter(|t| stats.rare.contains(*t)) {
            for &other in index.get(term.as_str()).into_iter().flatten() {
                if other != c {
                    *scores.entry(other).or_default() += stats.idf.get(term).copied().unwrap_or(0.0);
                }
            }
        }
        if scores.is_empty() {
            continue;
        }
        let mut ranked: Vec<(usize, f64)> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        out.push(AiRequest { market: m, clusters: ranked.into_iter().take(MAX_CANDIDATES).map(|(c, _)| c).collect() });
    }
    out.sort_by(|a, b| profiles[b.market].volume.cmp(&profiles[a.market].volume).then(profiles[a.market].key.cmp(&profiles[b.market].key)));
    out
}

/// The clusters the rules left as one lone market: what a model may be asked about.
pub fn singletons(clusters: &[Cluster]) -> Vec<usize> {
    clusters.iter().enumerate().filter(|(_, c)| c.members.len() == 1 && c.reason == Reason::Single).map(|(i, _)| i).collect()
}

fn apply_hints(units: &mut Vec<Unit>, profiles: &[Profile], hints: &HashMap<String, Hint>) {
    if hints.is_empty() {
        return;
    }
    // Only a lone market takes a hint, and only once.
    let mut lone: Vec<usize> = units.iter().enumerate().filter(|(_, u)| u.members.len() == 1 && u.reason == Reason::Single).map(|(i, _)| i).collect();
    lone.sort_by(|&a, &b| units[a].key.cmp(&units[b].key));

    let mut remove: BTreeSet<usize> = BTreeSet::new();
    let mut joins: Vec<(usize, usize)> = Vec::new(); // (lone unit, host unit)
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for &i in &lone {
        let Some(hint) = hints.get(&profiles[units[i].members[0]].key) else { continue };
        if let Some(join) = &hint.join
            && let Some(host) = units.iter().position(|u| &u.key == join)
            && host != i
        {
            joins.push((i, host));
        } else if let Some(group) = &hint.group {
            groups.entry(group.clone()).or_default().push(i);
        }
    }
    for (i, host) in joins {
        if remove.contains(&i) || remove.contains(&host) || units[host].members.len() >= MAX_MEMBERS || units_conflict(&units[i], &units[host], profiles) {
            continue;
        }
        let member = units[i].members[0];
        units[host].members.push(member);
        units[host].members.sort_by(|&a, &b| profiles[b].volume.cmp(&profiles[a].volume).then(profiles[a].key.cmp(&profiles[b].key)));
        if units[host].reason == Reason::Single {
            units[host].reason = Reason::Ai;
            units[host].key = format!("ai:{}", profiles[units[host].members[0]].key);
            units[host].detail = "A model judged these to be about the same subject.".to_string();
            units[host].confidence = 0.7;
        }
        remove.insert(i);
    }
    let mut created: Vec<Unit> = Vec::new();
    for (group, indexes) in groups {
        let indexes: Vec<usize> = indexes.into_iter().filter(|i| !remove.contains(i)).collect();
        if indexes.len() < 2 {
            continue;
        }
        // Keep the largest subset with no pairwise conflict, taking markets in volume order.
        let mut chosen: Vec<usize> = Vec::new();
        for &i in &indexes {
            if chosen.iter().all(|&c| !units_conflict(&units[i], &units[c], profiles)) && chosen.len() < MAX_MEMBERS {
                chosen.push(i);
            }
        }
        if chosen.len() < 2 {
            continue;
        }
        let mut members: Vec<usize> = chosen.iter().map(|&i| units[i].members[0]).collect();
        members.sort_by(|&a, &b| profiles[b].volume.cmp(&profiles[a].volume).then(profiles[a].key.cmp(&profiles[b].key)));
        let title = chosen.iter().find_map(|&i| hints.get(&profiles[units[i].members[0]].key).and_then(|h| h.title.clone()));
        remove.extend(chosen);
        created.push(Unit {
            key: format!("ai:{group}:{}", profiles[members[0]].key),
            members,
            reason: Reason::Ai,
            detail: "A model judged these to be about the same subject.".to_string(),
            confidence: 0.7,
            title,
        });
    }
    let kept: Vec<Unit> = std::mem::take(units).into_iter().enumerate().filter(|(i, _)| !remove.contains(i)).map(|(_, u)| u).collect();
    *units = kept;
    units.extend(created);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::dictionary::Dictionary;
    use crate::engine::subject::profile;
    use crate::models::venue::{Category, VenueEvent, VenueMarket};

    const NOW: i64 = 1_789_000_000;

    fn market(venue: &str, id: &str, title: &str, event: Option<(&str, &str)>, volume: u32) -> VenueMarket {
        VenueMarket {
            venue_id: venue.into(),
            venue_market_id: id.into(),
            title: title.into(),
            event: event.map(|(eid, t)| VenueEvent { event_id: eid.into(), title: t.into(), ..Default::default() }),
            category: Category::from("culture"),
            close_time: NOW + 2 * 86_400,
            liquidity: Some(Decimal::from(volume)),
            ..Default::default()
        }
    }

    fn run(markets: &[VenueMarket], hints: &HashMap<String, Hint>) -> (Vec<Profile>, Vec<Cluster>) {
        let dictionary = Dictionary::seeded();
        let profiles: Vec<Profile> = markets.iter().map(|m| profile(m, &dictionary)).collect();
        let clusters = cluster(&profiles, hints);
        (profiles, clusters)
    }

    fn keys_of(profiles: &[Profile], cluster: &Cluster) -> Vec<String> {
        let mut keys: Vec<String> = cluster.members.iter().map(|&m| profiles[m].key.clone()).collect();
        keys.sort();
        keys
    }

    /// A catalog's worth of unrelated markets, so rare words are rare.
    fn filler(n: usize) -> Vec<VenueMarket> {
        (0..n)
            .map(|i| market("panta", &format!("f{i}"), &format!("Will filler{i} subject{i} happen on topic{i}?"), None, 1))
            .collect()
    }

    #[test]
    fn two_bbnaija_markets_with_different_spellings_become_one_event() {
        let mut markets = vec![
            market("panta", "tram", "Will Tram finish in the Top 3 of BBNaija Season 11?", None, 5),
            market("panta", "female", "A female housemate to win Big Brother Naija Season 11?", None, 400),
        ];
        markets.extend(filler(30));
        let (profiles, clusters) = run(&markets, &HashMap::new());
        let bb = clusters.iter().find(|c| keys_of(&profiles, c).contains(&"panta:tram".to_string())).unwrap();
        assert_eq!(keys_of(&profiles, bb), vec!["panta:female".to_string(), "panta:tram".to_string()]);
        assert_eq!(bb.reason, Reason::Terms);
        assert_eq!(profiles[bb.members[0]].key, "panta:female", "the most-traded market leads");
    }

    #[test]
    fn a_different_season_is_a_different_event() {
        let mut markets = vec![
            market("panta", "a", "Will Tram finish in the Top 3 of BBNaija Season 11?", None, 5),
            market("panta", "b", "Will Ada win Big Brother Naija Season 10?", None, 5),
        ];
        markets.extend(filler(30));
        let (profiles, clusters) = run(&markets, &HashMap::new());
        let with = |k: &str| clusters.iter().find(|c| keys_of(&profiles, c).contains(&k.to_string())).unwrap();
        assert_ne!(with("panta:a").key, with("panta:b").key);
    }

    #[test]
    fn unrelated_markets_stay_single_events() {
        let (_, clusters) = run(&filler(20), &HashMap::new());
        assert_eq!(clusters.len(), 20);
        assert!(clusters.iter().all(|c| c.reason == Reason::Single));
    }

    #[test]
    fn markets_a_venue_put_in_one_event_stay_together_with_that_reason() {
        let markets = vec![
            market("jupiter", "m1", "Option A", Some(("E1", "Which option wins?")), 10),
            market("jupiter", "m2", "Option B", Some(("E1", "Which option wins?")), 20),
            market("jupiter", "m3", "Other thing", Some(("E2", "Something else entirely")), 5),
        ];
        let (profiles, clusters) = run(&markets, &HashMap::new());
        let e1 = clusters.iter().find(|c| c.members.len() == 2).unwrap();
        assert_eq!(e1.reason, Reason::VenueEvent);
        assert_eq!(keys_of(&profiles, e1), vec!["jupiter:m1".to_string(), "jupiter:m2".to_string()]);
    }

    #[test]
    fn a_hint_joins_a_lone_market_to_an_existing_cluster_but_not_across_a_conflict() {
        let markets = vec![
            market("jupiter", "m1", "Option A", Some(("E1", "Show Season 5 winner")), 10),
            market("jupiter", "m2", "Option B", Some(("E1", "Show Season 5 winner")), 20),
            market("panta", "x", "Will a quiet one win the show Season 5?", None, 1),
            market("panta", "y", "Will a quiet one win the show Season 6?", None, 1),
        ];
        let (profiles, base) = run(&markets, &HashMap::new());
        let host = base.iter().find(|c| c.members.len() == 2).unwrap().key.clone();
        let mut hints = HashMap::new();
        hints.insert("panta:x".to_string(), Hint { join: Some(host.clone()), ..Default::default() });
        hints.insert("panta:y".to_string(), Hint { join: Some(host.clone()), ..Default::default() });
        let (profiles2, clusters) = run(&markets, &hints);
        let joined = clusters.iter().find(|c| c.members.len() == 3).expect("x joined the cluster");
        assert!(keys_of(&profiles2, joined).contains(&"panta:x".to_string()));
        assert!(!keys_of(&profiles2, joined).contains(&"panta:y".to_string()), "a different season is refused");
        let _ = profiles;
    }

    #[test]
    fn markets_a_model_groups_form_a_new_titled_event() {
        let markets = vec![
            market("panta", "p", "Pineapple sells out in Lagos?", None, 3),
            market("panta", "q", "Mango is cheaper than pineapple?", None, 2),
        ];
        let mut hints = HashMap::new();
        for id in ["panta:p", "panta:q"] {
            hints.insert(id.to_string(), Hint { group: Some("g1".into()), title: Some("Fruit prices".into()), ..Default::default() });
        }
        let (_, clusters) = run(&markets, &hints);
        let ai = clusters.iter().find(|c| c.reason == Reason::Ai).expect("a model-grouped event");
        assert_eq!(ai.members.len(), 2);
        assert_eq!(ai.title.as_deref(), Some("Fruit prices"));
    }

    #[test]
    fn clustering_does_not_depend_on_input_order() {
        let mut markets = vec![
            market("panta", "tram", "Will Tram finish in the Top 3 of BBNaija Season 11?", None, 5),
            market("panta", "female", "A female housemate to win Big Brother Naija Season 11?", None, 400),
        ];
        markets.extend(filler(30));
        let (pa, a) = run(&markets, &HashMap::new());
        markets.reverse();
        let (pb, b) = run(&markets, &HashMap::new());
        let shape = |p: &[Profile], c: &[Cluster]| {
            let mut v: Vec<Vec<String>> = c.iter().map(|c| keys_of(p, c)).collect();
            v.sort();
            v
        };
        assert_eq!(shape(&pa, &a), shape(&pb, &b));
    }
}
