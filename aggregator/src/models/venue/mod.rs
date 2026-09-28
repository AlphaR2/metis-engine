//! Venue-facing data shapes shared by every `services::<vendor>` integration, plus each vendor's
//! own wire-format models in `panta`, `jupiter`, `pascal`. Error types live in `crate::errors`,
//! not here - this module is pure data.

pub mod jupiter;
pub mod panta;
pub mod pascal;
pub mod timestamp;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A category name, normalised for comparison: trimmed and lowercased. **Not a closed set** -
/// categories come from the venues (each venue's own list plus whatever its markets report), so a
/// category a venue adds tomorrow shows up without a code change.
///
/// Two spellings of one idea across venues (Jupiter `climate & science`, Panta `science`) are
/// reconciled by `CategoryAliases`, which is data, not code. What the venue actually sent is kept
/// verbatim on `VenueMarket::venue_category`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(from = "String", into = "String")]
pub struct Category(String);

impl Category {
    pub fn new(raw: &str) -> Self {
        Self(raw.trim().to_lowercase())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for Category {
    fn from(raw: String) -> Self {
        Self::new(&raw)
    }
}

impl From<&str> for Category {
    fn from(raw: &str) -> Self {
        Self::new(raw)
    }
}

impl From<Category> for String {
    fn from(category: Category) -> Self {
        category.0
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Maps a venue's category name onto the shared name used across venues, where the two differ
/// only in wording. Anything without an alias is its own shared name, so an unaliased category is
/// still served - it is just not merged with another venue's.
#[derive(Debug, Clone, Default)]
pub struct CategoryAliases {
    aliases: std::collections::HashMap<Category, Category>,
}

impl CategoryAliases {
    /// The aliases known to be needed today, from the Phase 0 field inventory
    /// (`docs/venue-fields.md`). Extend with `CATEGORY_ALIASES` rather than code.
    pub fn defaults() -> Self {
        Self::from_pairs([("climate & science", "science"), ("tech", "technology"), ("pop-culture", "culture")])
    }

    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let aliases = pairs.into_iter().map(|(from, to)| (Category::new(from), Category::new(to))).collect();
        Self { aliases }
    }

    /// Parses `from=to,from=to` (e.g. from the `CATEGORY_ALIASES` env var) on top of `defaults()`.
    pub fn with_overrides(mut self, spec: &str) -> Self {
        for pair in spec.split(',') {
            if let Some((from, to)) = pair.split_once('=')
                && !from.trim().is_empty()
                && !to.trim().is_empty()
            {
                self.aliases.insert(Category::new(from), Category::new(to));
            }
        }
        self
    }

    /// The shared name for `native`.
    pub fn canonical(&self, native: &str) -> Category {
        let category = Category::new(native);
        self.aliases.get(&category).cloned().unwrap_or(category)
    }
}

/// How a venue frames a binary proposition. Panta: literal yes/no tokens. Pascal: no yes/no at
/// all — you're long or short a contract that settles to a price in [$0, $1]. The canonical layer
/// normalizes across this; adapters report their own native framing so nothing is lost translating
/// into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutcomeFraming {
    YesNo,
    LongShort,
}

/// Whether a venue supports unattended trading. This is the concrete answer to "can an AI/bot use
/// this venue without a human clicking approve on every trade" — a first-class, queryable property
/// rather than an assumption baked into the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradingModel {
    /// Caller's own wallet must sign every transaction (Panta's model) - human-approval-shaped.
    UserSignedPerTrade,
    /// A separate, revocable delegated key signs trades autonomously (Pascal's model) - suitable
    /// for unattended/automated/AI-driven trading.
    DelegatedTradingKey,
}

/// What a venue can do, queried once per venue rather than assumed.
///
/// Note this is currently static per-adapter, not per-market: Panta markets carry a `primary`
/// (bonding-curve) phase and a `secondary` phase (`secondaryYesPrice`/`secondaryNoPrice` appear
/// in its catalog response once a market transitions), but Panta does not expose a secondary-
/// market trading API today - confirmed directly, not just undocumented. `has_order_book: false`
/// is therefore correct for every phase Panta actually lets us quote/trade against.
/// `PantaAdapter::quote()` always calls the primary endpoint; a market that has moved to
/// `secondary` simply can't be quoted through Panta via this integration until they ship that
/// API, at which point this capability (and `PantaAdapter`) would need revisiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VenueCapabilities {
    pub framing: OutcomeFraming,
    pub trading_model: TradingModel,
    /// false for a bonding-curve venue like Panta (one quote endpoint gives you a direct fill
    /// estimate); true for an order-book venue like Pascal (a quote means walking the book).
    pub has_order_book: bool,
    /// true if the venue honours a server-side `category` filter on `list_markets`, with the
    /// names `VenueAdapter::filter_categories` returns. False means the refresh fetches the whole
    /// catalog in one unfiltered sweep.
    pub supports_category_filter: bool,
    /// false if the venue's cursor pagination does not actually advance - the symptom is
    /// `nextCursor` coming back constant, so following it re-serves page 1 indefinitely (this is
    /// exactly what Panta did until it was fixed venue-side on 2026-09-28; verified live in
    /// `scripts/check-panta.sh`). The refresh detects an unchanged cursor generically regardless
    /// of this flag, but recording it here keeps the behaviour visible rather than buried as a
    /// surprise.
    pub cursor_pagination_advances: bool,
    /// true if this venue can build a buy transaction for us to hand to a wallet. A UI asks this
    /// rather than assuming, so a venue we can only read stays visibly read-only.
    pub can_execute: bool,
    /// true if this venue lets a caller create new markets (Panta today).
    pub can_create_markets: bool,
}

/// A venue's own parent event for a market, preserved as the venue reports it.
///
/// This is **not** the canonical cross-venue event graph in `models::event` - nothing here is
/// matched or merged across venues. It is simply the grouping the venue itself publishes, and
/// carrying it is the only way a market reads as a question rather than a fragment.
///
/// Jupiter nests markets under events: the event holds the question ("Atlanta Dream vs New York
/// Liberty") while each market holds only its outcome ("Atlanta Dream"). Dropping the event, as
/// this integration previously did, is what reduced a whole catalog to labels like "Up", "Down",
/// "160-179" and "September 21" with no way to tell what was being predicted.
///
/// `None` for a venue with no native event layer - Panta puts the full question on the market
/// itself, so there is nothing to nest.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct VenueEvent {
    /// The venue's own id for the event.
    pub event_id: String,
    /// The question being asked, e.g. "Which model is best at the end of September?".
    pub title: String,
    /// Extra qualifying text the venue shows beneath the title, when it has any.
    pub subtitle: Option<String>,
    /// Event artwork, for a UI that wants it.
    pub image_url: Option<String>,
    /// The venue's URL slug for the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// Longer description, when the venue has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The venue's own tags, verbatim (Jupiter: `elections`, `middle-east`, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The venue's subcategory, verbatim - league or asset class (Jupiter: `mlb`, `cs2`, `btc`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subcategory: Option<String>,
    /// League or competition name, when the venue names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub league: Option<String>,
    /// When the real-world event starts (kickoff), as distinct from when its markets opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    /// Total traded volume across the event, in USDC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub volume_usdc: Option<Decimal>,
    /// Traded volume in the last 24 hours, in USDC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub volume_24h_usdc: Option<Decimal>,
    /// The venue's plain-language close condition, e.g. "closes after a winner is declared".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_condition: Option<String>,
    /// Every other field the venue sent for the event, untouched, so nothing is lost between the
    /// wire and our store even before we model it.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    #[schema(value_type = Object)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A team, player or other named participant a venue attaches to a market.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VenueEntity {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abbreviation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
}

/// One market as a venue reports it, before any canonical normalization. This is what
/// `GET /markets` serves verbatim - no transform into a canonical event shape.
///
/// Three distinct time axes, deliberately kept separate because venues genuinely report them
/// separately and conflating them produces wrong answers:
/// - `start_time` — when the market opened (Panta `startTime`, Jupiter `openTime`).
/// - `close_time` — when it **stops accepting bets** (Panta `endTime`, Jupiter `closeTime`).
/// - `resolution_time` — when the outcome is known/settled (Panta `resolutionTime`).
///
/// Close and resolution are not the same moment: on a 50-market Panta sample they differed on 38
/// of them, by anything from hours to five days. Betting windows key off `close_time`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct VenueMarket {
    /// Which venue this market came from, e.g. "panta". Set by the adapter that produced it - a
    /// market served over the API is meaningless without knowing where it can be traded.
    #[serde(default)]
    pub venue_id: String,
    pub venue_market_id: String,
    /// What this market is, in the venue's own words.
    ///
    /// For a venue that nests markets under events (Jupiter) this is the **outcome label** -
    /// "Atlanta Dream", "Up", "160-179" - and `event.title` carries the question. For a venue
    /// that publishes the whole question per market (Panta) this is that question and `event` is
    /// `None`. A UI should lead with `event.title` where there is one and fall back to this.
    pub title: String,
    /// The market's title readable on its own: `"{event title} — {title}"` when `title` is only an
    /// outcome label under a parent event, otherwise `title`. Set at ingestion; a UI can lead with
    /// it and never show a bare "Up" or "160-179".
    #[serde(default)]
    pub display_title: String,
    /// The venue's parent event. Venues without an event layer (Panta) get one event per market,
    /// so every market has an event to hang metadata on.
    #[serde(default)]
    pub event: Option<VenueEvent>,
    /// The venue's category for this market exactly as sent. `category` is the shared name after
    /// aliasing (see `CategoryAliases`).
    #[serde(default)]
    pub venue_category: String,
    /// The outcome labels the venue lists for this market, e.g. `["Falcons", "Packers"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outcome_labels: Vec<String>,
    /// The participant this market is about, when the venue says (Jupiter's `team`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<VenueEntity>,
    /// The venue's own market kind, verbatim (Jupiter `sportsMarketType`: `moneyline`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_kind: Option<String>,
    /// Where the venue itself sources the market from, when it aggregates (Jupiter: `polymarket`,
    /// `kalshi`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Every other field the venue sent for the market, untouched.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    #[schema(value_type = Object)]
    pub extra: serde_json::Map<String, serde_json::Value>,
    /// The venue's resolution terms in full - what counts as a win, and from which source.
    /// Surfaced on the market detail page, where it is the most important text on the screen.
    #[serde(default)]
    pub rules: Option<String>,
    #[schema(value_type = String)]
    pub category: Category,
    /// Venue-native phase/status string (e.g. Panta's "primary"/"secondary", kept verbatim for
    /// debugging rather than forced into a shared enum that would lose venue-specific meaning).
    pub phase: String,
    pub start_time: i64,
    /// When the market stops accepting bets. Falls back to `resolution_time` for a venue that
    /// genuinely reports no separate cutoff - never silently, always via that venue's model.
    #[serde(default)]
    pub close_time: i64,
    pub resolution_time: i64,
    pub yes_price: Option<String>,
    pub no_price: Option<String>,
    /// Traded volume in USDC as the venue reports it (Panta `volumeUsdc`, Jupiter
    /// `pricing.volume`) - the catalog-level depth signal `sort=best_liquidity` ranks on. `None`
    /// when the venue doesn't report one, and those markets always sort last rather than being
    /// treated as zero-volume.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub liquidity: Option<Decimal>,
    /// Whether the venue itself says this market can no longer take bets - distinct from `phase`,
    /// which on Panta is a separate wire field that can (rarely) disagree with this one. Defaults
    /// true only when a venue's own data says so; unknown/unverified venues default to false
    /// rather than silently hiding every market they have.
    #[serde(default)]
    pub dead: bool,
    /// Open, but not tradable through the venue's API - shown with its prices, never routed to.
    /// Panta after its primary phase: the market keeps trading on Panta's on-chain order book,
    /// which its API does not reach.
    #[serde(default)]
    pub display_only: bool,
}

impl VenueMarket {
    /// Quality gate applied before a market is stored or matched: not dead, still inside its
    /// betting window, and has something to show. Venues (Panta especially) return stale rows
    /// still flagged open, so `dead` alone isn't enough.
    ///
    /// The betting-window check is on `close_time`, not `resolution_time`: a market whose bets
    /// have closed but which hasn't settled yet is not bettable, and keying off resolution would
    /// keep showing it. A `close_time`/`resolution_time` of 0 means "unknown" (a garbled or
    /// missing timestamp) and is given the benefit of the doubt rather than dropped.
    pub fn is_bettable(&self, now: i64) -> bool {
        self.is_open(now) && !self.title.trim().is_empty()
    }

    /// `is_bettable` without the text check: the venue still takes bets on it, whether or not we
    /// have words to show yet. Lets an adapter decide which markets are worth fetching text for.
    pub fn is_open(&self, now: i64) -> bool {
        !self.dead
            && (self.close_time == 0 || self.close_time > now)
            && (self.resolution_time == 0 || self.resolution_time > now)
    }

    /// See `display_title`. Computed from the venue fields so ingestion can set it once.
    pub fn compose_display_title(&self) -> String {
        let title = self.title.trim();
        let Some(event) = self.event.as_ref().filter(|e| !e.title.trim().is_empty()) else {
            return title.to_string();
        };
        let event_title = event.title.trim();
        if title.is_empty() || title.eq_ignore_ascii_case(event_title) {
            event_title.to_string()
        } else {
            format!("{event_title} — {title}")
        }
    }

    /// The price for one side, as the venue quoted it in its catalog listing.
    pub fn price_for(&self, side: &str) -> Option<&str> {
        match side {
            "no" | "short" => self.no_price.as_deref(),
            _ => self.yes_price.as_deref(),
        }
    }
}

/// Optional filters passed to `list_markets` - a venue may ignore any of these.
#[derive(Debug, Clone, Default)]
pub struct ListParams {
    /// The venue's own category name, verbatim - one of the names its `filter_categories`
    /// returned. `None` lists every category.
    pub category: Option<String>,
    pub cursor: Option<String>,
    /// Venue-native sort field, e.g. Jupiter's `"beginAt"` - ignored by a venue that doesn't
    /// support server-side sorting.
    pub sort_by: Option<String>,
    /// Venue-native sort direction, e.g. `"asc"` - paired with `sort_by`.
    pub sort_direction: Option<String>,
    /// Venue-native named filter, e.g. Jupiter's `"upcoming"` - ignored by a venue that doesn't
    /// support it.
    pub filter: Option<String>,
}

/// One page of a venue's markets, with a cursor for the next page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketPage {
    pub items: Vec<VenueMarket>,
    pub next_cursor: Option<String>,
}

/// A single-venue quote for a given side/amount — the thing the position engine compares across
/// venues. `side` is kept as the venue's own native string (e.g. "yes" or "long") since the
/// canonical layer, not the adapter, owns translating a normalized side into venue-native terms.
/// Money fields are `Decimal`, not `f64` - exact fixed-point, no float rounding error, and
/// serializes as a JSON string (via the `serde-str` feature) matching this API's existing
/// string-money discipline everywhere else.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueQuote {
    pub venue: &'static str,
    pub venue_market_id: String,
    pub side: String,
    pub amount_usdc: Decimal,
    pub effective_shares_or_size: Decimal,
    pub avg_price: Decimal,
    pub fee_usdc: Decimal,
    /// Whether the venue could fill the *whole* requested amount. For an order-book venue this is
    /// `walk_book`'s own verdict, carried through rather than re-derived downstream from rounded
    /// size/price - re-multiplying two 6-dp values and comparing against the request drifts, and
    /// the book walk already knows the exact answer.
    #[serde(default)]
    pub fully_filled: bool,
    pub expires_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market(close_time: i64, resolution_time: i64) -> VenueMarket {
        VenueMarket {
            venue_id: "panta".to_string(),
            venue_market_id: "m1".to_string(),
            title: "Something vs Something".to_string(),
            event: None,
            rules: None,
            category: Category::from("sports"),
            phase: "primary".to_string(),
            start_time: 0,
            close_time,
            resolution_time,
            yes_price: Some("0.5".to_string()),
            no_price: Some("0.5".to_string()),
            liquidity: None,
            dead: false,
            ..Default::default()
        }
    }

    const NOW: i64 = 1_000_000;

    #[test]
    fn a_market_inside_its_betting_window_is_bettable() {
        assert!(market(NOW + 100, NOW + 500).is_bettable(NOW));
    }

    #[test]
    fn a_market_past_its_close_is_not_bettable_even_though_it_has_not_resolved() {
        // The whole point of tracking close_time separately: bets shut before settlement.
        assert!(!market(NOW - 100, NOW + 5_000).is_bettable(NOW));
    }

    #[test]
    fn an_unknown_close_time_is_given_the_benefit_of_the_doubt() {
        assert!(market(0, NOW + 500).is_bettable(NOW));
    }

    #[test]
    fn an_untitled_market_is_not_bettable() {
        let mut m = market(NOW + 100, NOW + 500);
        m.title = "   ".to_string();
        assert!(!m.is_bettable(NOW));
    }

    #[test]
    fn categories_compare_case_and_space_insensitively() {
        // Two venues spelling the same category differently must compare equal, or a required
        // `category` filter silently misses.
        assert_eq!(Category::from(" World "), Category::from("world"));
    }

    #[test]
    fn any_venue_category_is_accepted_not_just_a_known_list() {
        assert_eq!(Category::from("Quidditch").as_str(), "quidditch");
    }

    #[test]
    fn aliases_merge_differently_worded_categories_and_leave_the_rest_alone() {
        let aliases = CategoryAliases::defaults();
        assert_eq!(aliases.canonical("Climate & Science"), Category::from("science"));
        assert_eq!(aliases.canonical("science"), Category::from("science"));
        assert_eq!(aliases.canonical("mentions"), Category::from("mentions"));
    }

    #[test]
    fn alias_overrides_extend_the_defaults() {
        let aliases = CategoryAliases::defaults().with_overrides("stocks=finance, bad-entry ,=x");
        assert_eq!(aliases.canonical("stocks"), Category::from("finance"));
        assert_eq!(aliases.canonical("climate & science"), Category::from("science"));
    }

    #[test]
    fn price_for_picks_the_requested_side() {
        let m = market(NOW + 100, NOW + 500);
        assert_eq!(m.price_for("yes"), Some("0.5"));
        assert_eq!(m.price_for("no"), Some("0.5"));
    }
}
