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

/// A market's category, normalized across venues into a closed set so a UI can filter/group by
/// it. The two integrated venues do *not* share a vocabulary - confirmed live against both:
///
/// - Jupiter accepts `all|crypto|sports|politics|esports|culture|economics|tech|finance|
///   climate & science|weather|mentions` and 400s on anything else.
/// - Panta accepts `sports|crypto|politics|entertainment|finance|science|world|other`.
///
/// Only sports/crypto/politics/finance exist on both. This enum is the canonical superset a
/// caller filters by; each venue's own model module owns translating it into that venue's native
/// string (and saying "not supported here"), so a category one venue has never blocks the other.
///
/// `Other` preserves whatever raw string a venue sent that we don't recognize yet, rather than
/// dropping it - round-trips losslessly via `From<String>`/`Into<String>` (see the
/// `#[serde(from/into)]` below), so it still shows up correctly in JSON even if it's not one of
/// the named variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(from = "String", into = "String")]
pub enum Category {
    Crypto,
    Sports,
    Politics,
    Economics,
    Entertainment,
    Technology,
    Esports,
    Culture,
    Finance,
    /// Panta's `science`, Jupiter's `climate & science`.
    Science,
    World,
    Weather,
    /// Panta's catch-all bucket. A real, selectable category on that venue - distinct from
    /// `Unknown` below, which means we failed to recognise something.
    ///
    /// These were the same variant once, and the collision was a live bug: `"other"` fell into the
    /// catch-all, the catch-all mapped to "no equivalent", and Panta's `other` category became
    /// impossible to create in or browse.
    Other,
    /// Any venue-reported category string outside the known set above. Always stored lowercased
    /// so two venues spelling the same unknown category differently ("World" vs "world") compare
    /// equal - with `category` a *required* API filter, a case mismatch here is a silent miss,
    /// not a cosmetic wrinkle.
    Unknown(String),
}

impl From<String> for Category {
    fn from(raw: String) -> Self {
        let lowered = raw.to_lowercase();
        match lowered.trim() {
            "crypto" => Category::Crypto,
            "sports" => Category::Sports,
            "politics" => Category::Politics,
            "economics" => Category::Economics,
            "entertainment" => Category::Entertainment,
            "technology" | "tech" => Category::Technology,
            "esports" => Category::Esports,
            "culture" => Category::Culture,
            "finance" => Category::Finance,
            "science" | "climate & science" => Category::Science,
            "world" => Category::World,
            "weather" => Category::Weather,
            "other" => Category::Other,
            unrecognised => Category::Unknown(unrecognised.to_string()),
        }
    }
}

impl Category {
    /// Every category a caller may filter by, for `GET /categories` and OpenAPI docs.
    ///
    /// `Other` is included because it is a genuine, selectable bucket on Panta. `Unknown` is not:
    /// it exists to preserve venue output we failed to recognise, which is not something to offer
    /// as a choice.
    pub fn all() -> Vec<Category> {
        vec![
            Category::Sports,
            Category::Crypto,
            Category::Politics,
            Category::Economics,
            Category::Finance,
            Category::Technology,
            Category::Esports,
            Category::Culture,
            Category::Entertainment,
            Category::Science,
            Category::World,
            Category::Weather,
            Category::Other,
        ]
    }
}

impl From<Category> for String {
    fn from(category: Category) -> Self {
        match category {
            Category::Crypto => "crypto".to_string(),
            Category::Sports => "sports".to_string(),
            Category::Politics => "politics".to_string(),
            Category::Economics => "economics".to_string(),
            Category::Entertainment => "entertainment".to_string(),
            Category::Technology => "technology".to_string(),
            Category::Esports => "esports".to_string(),
            Category::Culture => "culture".to_string(),
            Category::Finance => "finance".to_string(),
            Category::Science => "science".to_string(),
            Category::World => "world".to_string(),
            Category::Weather => "weather".to_string(),
            Category::Other => "other".to_string(),
            Category::Unknown(raw) => raw,
        }
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
    /// true if the venue honours a server-side `category` filter on `list_markets`. Both
    /// integrated venues do - on different vocabularies, which `VenueAdapter::native_category`
    /// translates. False means the refresh fetches the whole catalog once and buckets by each
    /// market's own reported category.
    pub supports_category_filter: bool,
    /// false if the venue's cursor pagination does not actually advance. Panta's `nextCursor` is
    /// a constant: following it re-serves page 1 indefinitely (six requests returned 300 rows
    /// holding 50 distinct markets). The refresh detects an unchanged cursor generically, but
    /// recording it here keeps the behaviour visible rather than buried as a surprise.
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
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VenueEvent {
    /// The venue's own id for the event.
    pub event_id: String,
    /// The question being asked, e.g. "Which model is best at the end of September?".
    pub title: String,
    /// Extra qualifying text the venue shows beneath the title, when it has any.
    pub subtitle: Option<String>,
    /// Event artwork, for a UI that wants it.
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
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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
    /// The venue's own parent event, when it has one. See `VenueEvent`.
    #[serde(default)]
    pub event: Option<VenueEvent>,
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
        !self.dead
            && (self.close_time == 0 || self.close_time > now)
            && (self.resolution_time == 0 || self.resolution_time > now)
            && !self.title.trim().is_empty()
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
    pub category: Option<Category>,
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
            category: Category::Sports,
            phase: "primary".to_string(),
            start_time: 0,
            close_time,
            resolution_time,
            yes_price: Some("0.5".to_string()),
            no_price: Some("0.5".to_string()),
            liquidity: None,
            dead: false,
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
    fn category_parsing_is_case_insensitive_for_unknown_values_too() {
        // Two venues spelling the same unknown category differently must compare equal, or a
        // required `category` filter silently misses.
        assert_eq!(Category::from("World".to_string()), Category::World);
        assert_eq!(Category::from("MysteryCat".to_string()), Category::from("mysterycat".to_string()));
    }

    #[test]
    fn other_is_a_real_category_not_the_catch_all() {
        // Panta has a genuine bucket called "other". It shared a variant with the unrecognised
        // catch-all once, which made it impossible to create in or browse - the catch-all maps to
        // "this venue has no equivalent".
        assert_eq!(Category::from("other".to_string()), Category::Other);
        assert_ne!(Category::from("other".to_string()), Category::Unknown("other".to_string()));
    }

    #[test]
    fn an_unrecognised_category_is_unknown_not_other() {
        assert_eq!(
            Category::from("quidditch".to_string()),
            Category::Unknown("quidditch".to_string())
        );
    }

    #[test]
    fn other_is_offered_as_a_filterable_category() {
        // If it isn't in all(), it never reaches GET /categories and nobody can browse it.
        assert!(Category::all().contains(&Category::Other));
    }

    #[test]
    fn the_catch_all_is_never_offered_as_a_choice() {
        assert!(!Category::all().iter().any(|c| matches!(c, Category::Unknown(_))));
    }

    #[test]
    fn jupiters_climate_and_science_maps_onto_science() {
        assert_eq!(Category::from("climate & science".to_string()), Category::Science);
    }

    #[test]
    fn category_round_trips_through_string() {
        for category in Category::all() {
            let raw: String = category.clone().into();
            assert_eq!(Category::from(raw), category);
        }
    }

    #[test]
    fn price_for_picks_the_requested_side() {
        let m = market(NOW + 100, NOW + 500);
        assert_eq!(m.price_for("yes"), Some("0.5"));
        assert_eq!(m.price_for("no"), Some("0.5"));
    }
}
