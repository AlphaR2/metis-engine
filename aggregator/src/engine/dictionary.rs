//! The entity dictionary: which names in market text refer to the same real thing.
//!
//! Seeded with the entities that recur across prediction markets (crypto assets, commodities,
//! indices, large stocks, countries), and grown from venue data at run time - Jupiter tags team
//! markets with `{name, abbreviation}`, which teaches "Falcons" and "ATL" = Atlanta Falcons. This is
//! data, not venue-specific code: any venue's structured entities feed it the same way.

use std::collections::HashMap;

use serde::Serialize;
use utoipa::ToSchema;

use super::text::{fold, slug};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum EntityKind {
    Asset,
    Commodity,
    Index,
    Stock,
    Team,
    Person,
    Country,
    Org,
}

/// A resolved reference to one real-world entity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, ToSchema)]
pub struct EntityRef {
    /// Stable key, e.g. `bitcoin`, `atlanta-falcons`.
    pub key: String,
    /// Display name, e.g. "Bitcoin".
    pub name: String,
    pub kind: EntityKind,
}

#[derive(Debug, Clone)]
struct Alias {
    entity: EntityRef,
    /// Only match when written in capitals ("SOL", not "sol"), for tickers that are also words.
    uppercase_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Dictionary {
    /// Folded alias (words joined by single spaces) → entity.
    aliases: HashMap<String, Alias>,
    longest_alias_words: usize,
}

/// (display name, kind, aliases). Aliases in capitals are matched only when written in capitals.
const SEED: &[(&str, EntityKind, &[&str])] = &[
    ("Bitcoin", EntityKind::Asset, &["bitcoin", "btc", "BTC"]),
    ("Ethereum", EntityKind::Asset, &["ethereum", "ether", "eth", "ETH"]),
    ("Solana", EntityKind::Asset, &["solana", "SOL"]),
    ("XRP", EntityKind::Asset, &["xrp", "ripple"]),
    ("Dogecoin", EntityKind::Asset, &["dogecoin", "doge"]),
    ("Cardano", EntityKind::Asset, &["cardano", "ADA"]),
    ("BNB", EntityKind::Asset, &["bnb", "binance coin"]),
    ("Hyperliquid", EntityKind::Asset, &["hyperliquid", "HYPE"]),
    ("Arbitrum", EntityKind::Asset, &["arbitrum", "ARB"]),
    ("Sui", EntityKind::Asset, &["SUI"]),
    ("Avalanche", EntityKind::Asset, &["avalanche", "AVAX", "avax"]),
    ("Chainlink", EntityKind::Asset, &["chainlink", "LINK"]),
    ("Toncoin", EntityKind::Asset, &["toncoin", "TON"]),
    ("Litecoin", EntityKind::Asset, &["litecoin", "LTC"]),
    ("Tron", EntityKind::Asset, &["TRX", "tron"]),
    ("Pepe", EntityKind::Asset, &["PEPE"]),
    ("Gold", EntityKind::Commodity, &["gold", "xau"]),
    ("Silver", EntityKind::Commodity, &["silver", "xag"]),
    ("Crude oil", EntityKind::Commodity, &["crude oil", "oil", "wti", "brent"]),
    ("Natural gas", EntityKind::Commodity, &["natural gas"]),
    ("S&P 500", EntityKind::Index, &["s&p 500", "s&p", "sp500", "spx", "SPY"]),
    ("Nasdaq 100", EntityKind::Index, &["nasdaq 100", "nasdaq-100", "ndx", "QQQ"]),
    ("Dow Jones", EntityKind::Index, &["dow jones", "dow", "djia"]),
    ("Tesla", EntityKind::Stock, &["tesla", "tsla", "TSLA"]),
    ("Apple", EntityKind::Stock, &["apple", "aapl"]),
    ("Nvidia", EntityKind::Stock, &["nvidia", "nvda"]),
    ("Microsoft", EntityKind::Stock, &["microsoft", "msft"]),
    ("Amazon", EntityKind::Stock, &["amazon", "amzn"]),
    ("Alphabet", EntityKind::Stock, &["alphabet", "google", "googl"]),
    ("Meta", EntityKind::Stock, &["meta platforms", "META"]),
    ("Super Micro Computer", EntityKind::Stock, &["super micro computer", "super micro", "smci"]),
    ("Coinbase", EntityKind::Stock, &["coinbase", "COIN"]),
    ("MicroStrategy", EntityKind::Stock, &["microstrategy", "strategy inc", "mstr"]),
    ("OpenAI", EntityKind::Org, &["openai"]),
    ("Anthropic", EntityKind::Org, &["anthropic"]),
    ("Argentina", EntityKind::Country, &["argentina"]),
    ("Brazil", EntityKind::Country, &["brazil"]),
    ("England", EntityKind::Country, &["england"]),
    ("France", EntityKind::Country, &["france"]),
    ("Spain", EntityKind::Country, &["spain"]),
    ("Germany", EntityKind::Country, &["germany"]),
    ("Portugal", EntityKind::Country, &["portugal"]),
    ("Netherlands", EntityKind::Country, &["netherlands", "holland"]),
    ("Belgium", EntityKind::Country, &["belgium"]),
    ("Italy", EntityKind::Country, &["italy"]),
    ("Norway", EntityKind::Country, &["norway"]),
    ("Sweden", EntityKind::Country, &["sweden"]),
    ("Switzerland", EntityKind::Country, &["switzerland"]),
    ("Austria", EntityKind::Country, &["austria"]),
    ("Colombia", EntityKind::Country, &["colombia"]),
    ("Egypt", EntityKind::Country, &["egypt"]),
    ("Senegal", EntityKind::Country, &["senegal"]),
    ("Nigeria", EntityKind::Country, &["nigeria", "super eagles", "super falcons"]),
    ("Malawi", EntityKind::Country, &["malawi"]),
    ("Cote d'Ivoire", EntityKind::Country, &["cote d'ivoire", "ivory coast"]),
    ("DR Congo", EntityKind::Country, &["dr congo", "drc"]),
    ("United States", EntityKind::Country, &["united states", "usa", "u.s.", "US"]),
    ("Mexico", EntityKind::Country, &["mexico"]),
    ("Canada", EntityKind::Country, &["canada"]),
    ("Japan", EntityKind::Country, &["japan"]),
    ("China", EntityKind::Country, &["china"]),
    ("Taiwan", EntityKind::Country, &["taiwan"]),
    ("Russia", EntityKind::Country, &["russia"]),
    ("Ukraine", EntityKind::Country, &["ukraine"]),
    ("Israel", EntityKind::Country, &["israel"]),
    ("Iran", EntityKind::Country, &["iran"]),
    ("Venezuela", EntityKind::Country, &["venezuela"]),
    ("United Kingdom", EntityKind::Country, &["united kingdom", "UK", "britain"]),
];

impl Dictionary {
    /// The seeded dictionary.
    pub fn seeded() -> Self {
        let mut dictionary = Self::default();
        for (name, kind, aliases) in SEED {
            let entity = EntityRef { key: slug(name), name: name.to_string(), kind: *kind };
            dictionary.add(name, entity.clone());
            for alias in *aliases {
                dictionary.add(alias, entity.clone());
            }
        }
        dictionary
    }

    /// Registers `alias` for `entity`. An alias written entirely in capitals only matches text
    /// written in capitals. An alias that already names a *different* entity is left alone - the
    /// first registration wins, so a learned alias can never hijack a seeded one.
    pub fn add(&mut self, alias: &str, entity: EntityRef) {
        let uppercase_only = alias.len() <= 5 && alias.chars().all(|c| c.is_ascii_uppercase());
        let key = normalize_alias(alias);
        if key.is_empty() {
            return;
        }
        let words = key.split(' ').count();
        match self.aliases.get(&key) {
            Some(existing) if existing.entity.key != entity.key => {}
            Some(existing) if !existing.uppercase_only => {}
            _ => {
                self.longest_alias_words = self.longest_alias_words.max(words);
                self.aliases.insert(key, Alias { entity, uppercase_only });
            }
        }
    }

    /// Learns a named participant a venue attached to a market (Jupiter's `team`), under its
    /// name, its last word ("Atlanta Falcons" → "Falcons") and its abbreviation.
    pub fn learn_participant(&mut self, name: &str, abbreviation: Option<&str>, kind: EntityKind) -> EntityRef {
        if let Some(known) = self.lookup(name) {
            return known;
        }
        let entity = EntityRef { key: slug(name), name: name.trim().to_string(), kind };
        self.add(name, entity.clone());
        if let Some(last) = name.split_whitespace().last()
            && last.len() > 3
            && name.split_whitespace().count() > 1
        {
            self.add(last, entity.clone());
        }
        if let Some(abbreviation) = abbreviation.filter(|a| a.len() >= 2) {
            self.add(&abbreviation.to_uppercase(), entity.clone());
        }
        entity
    }

    /// The entity `name` refers to, if the whole of `name` is a known alias.
    pub fn lookup(&self, name: &str) -> Option<EntityRef> {
        let alias = self.aliases.get(&normalize_alias(name))?;
        (!alias.uppercase_only || name.trim().chars().any(|c| c.is_ascii_uppercase())).then(|| alias.entity.clone())
    }

    /// Every known entity mentioned in `text`, longest match first, each once.
    pub fn find_all(&self, text: &str) -> Vec<EntityRef> {
        let original: Vec<&str> = text
            .split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | '?' | '!' | ':' | ';' | '"' | '/'))
            .filter(|w| !w.is_empty())
            .collect();
        let folded: Vec<String> = original.iter().map(|w| normalize_alias(w)).collect();
        let mut found: Vec<EntityRef> = Vec::new();
        let mut i = 0;
        while i < folded.len() {
            let mut matched = 0;
            for n in (1..=self.longest_alias_words.min(folded.len() - i)).rev() {
                let candidate = folded[i..i + n].join(" ");
                if let Some(alias) = self.aliases.get(&candidate) {
                    let written_upper = original[i..i + n].iter().all(|w| {
                        let letters: Vec<char> = w.chars().filter(|c| c.is_alphabetic()).collect();
                        !letters.is_empty() && letters.iter().all(|c| c.is_uppercase())
                    });
                    if alias.uppercase_only && !written_upper {
                        continue;
                    }
                    if !found.contains(&alias.entity) {
                        found.push(alias.entity.clone());
                    }
                    matched = n;
                    break;
                }
            }
            i += matched.max(1);
        }
        found
    }
}

/// Folded, with possessives and surrounding punctuation dropped and inner whitespace collapsed.
fn normalize_alias(text: &str) -> String {
    let folded = fold(text);
    let words: Vec<String> = folded
        .split_whitespace()
        .map(|w| {
            let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '&' && c != '\'' && c != '.' && c != '-');
            let w = w.trim_end_matches("'s").trim_end_matches('\'').trim_end_matches('.');
            w.trim_start_matches('$').to_string()
        })
        .filter(|w| !w.is_empty())
        .collect();
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(found: Vec<EntityRef>) -> Vec<String> {
        found.into_iter().map(|e| e.key).collect()
    }

    #[test]
    fn finds_seeded_entities_by_name_and_ticker() {
        let d = Dictionary::seeded();
        assert_eq!(keys(d.find_all("Will Bitcoin (BTC) be priced at $81,000.00 or higher")), ["bitcoin"]);
        assert_eq!(keys(d.find_all("Will Tesla (NASDAQ: TSLA) close above $330.00")), ["tesla"]);
        assert_eq!(keys(d.find_all("Will Super Micro Computer's stock price ($SMCI) close")), ["super-micro-computer"]);
        assert_eq!(keys(d.find_all("Will the Norway vs. Côte d'Ivoire match")), ["norway", "cote-d-ivoire"]);
    }

    #[test]
    fn ambiguous_tickers_only_match_in_capitals() {
        let d = Dictionary::seeded();
        assert_eq!(keys(d.find_all("Will Solana (SOL) trade at or above $81.50")), ["solana"]);
        assert!(d.find_all("the sol rises; a link to the ton of news").is_empty());
        assert_eq!(keys(d.find_all("LINK hits $30")), ["chainlink"]);
    }

    #[test]
    fn learned_participants_are_found_by_name_nickname_and_abbreviation() {
        let mut d = Dictionary::seeded();
        let falcons = d.learn_participant("Atlanta Falcons", Some("atl"), EntityKind::Team);
        assert_eq!(falcons.key, "atlanta-falcons");
        assert_eq!(keys(d.find_all("Falcons vs Packers")), ["atlanta-falcons"]);
        assert_eq!(keys(d.find_all("ATL -3.5")), ["atlanta-falcons"]);
        assert_eq!(d.lookup("atlanta falcons"), Some(falcons));
    }

    #[test]
    fn a_learned_alias_never_hijacks_a_seeded_one() {
        let mut d = Dictionary::seeded();
        d.learn_participant("Gold Coast Suns", Some("GOLD"), EntityKind::Team);
        assert_eq!(keys(d.find_all("Will gold reach $5,000")), ["gold"]);
    }
}
