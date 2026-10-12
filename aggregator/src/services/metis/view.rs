//! Turning the program's accounts into what the aggregator shows: addresses, markets, quotes and
//! positions. Pure functions over decoded accounts - no network - so every rule is a unit test.
//!
//! **The one rule that runs through all of it: show what a bet actually pays, never the curve's
//! own probability.** The program mints shares off a curve with virtual liquidity, but pays winners
//! out of the real pot, so the curve's price is not the odds. Every price and size here is worked
//! out from `quote_bet`, which prices the payout, so a number shown to a user is a number they get.

use metis_core::{
    amm::{
        condition,
        metadata::MarketMetadata,
        quote::{BetQuote, quote_bet, rake_of},
        state::{Market, MarketStatus},
    },
    constants::{CONFIG_SEED, GUARDIAN, OUTCOME_MINT_SEED},
};
use rust_decimal::Decimal;
use solana_pubkey::Pubkey;

use super::secondary::{Books, D18};
use crate::models::venue::{Category, VenueEvent, VenueMarket};

const TOKEN_PROGRAM: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const ATA_PROGRAM: Pubkey = Pubkey::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

/// Football matches rarely last four hours from kickoff, even with extra time and a shoot-out.
/// The program records no resolution time (the oracle decides when a match is over), so this is
/// an honest upper estimate, labelled as one in `extra`.
const EXPECTED_RESOLUTION_SECS: i64 = 4 * 60 * 60;

pub const ID_YES: usize = 0;
pub const ID_NO: usize = 1;

// ── Addresses ────────────────────────────────────────────────────────────

fn pda(seeds: &[&[u8]], program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(seeds, program).0
}

pub fn outcome_mint(program: &Pubkey, market: &Pubkey, index: usize) -> Pubkey {
    pda(&[OUTCOME_MINT_SEED, market.as_ref(), &[index as u8]], program)
}

pub fn associated_token_account(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    pda(&[owner.as_ref(), TOKEN_PROGRAM.as_ref(), mint.as_ref()], &ATA_PROGRAM)
}

/// The Config PDA, from the trust roots this build was compiled with. Correct only if those match
/// the deployed program's, which is why `METIS_CONFIG` can override it.
pub fn default_config(program: &Pubkey) -> Pubkey {
    pda(&[CONFIG_SEED, GUARDIAN.as_ref()], program)
}

pub fn side_index(side: &str) -> Option<usize> {
    match side.trim().to_lowercase().as_str() {
        "yes" => Some(ID_YES),
        "no" => Some(ID_NO),
        _ => None,
    }
}

pub fn side_name(index: usize) -> &'static str {
    if index == ID_YES { "yes" } else { "no" }
}

// ── Money ────────────────────────────────────────────────────────────────

/// USDC atoms (6 decimals) as an exact decimal.
pub fn usdc(atoms: u64) -> Decimal {
    Decimal::new(atoms as i64, 6)
}

/// A decimal USDC amount as atoms, or `None` if it has sub-atom precision, is negative, or doesn't
/// fit. Refusing rather than rounding: the user must get exactly the amount they asked to spend.
pub fn to_atoms(amount: Decimal) -> Option<u64> {
    let scaled = amount.checked_mul(Decimal::from(1_000_000u64))?;
    if scaled.fract() != Decimal::ZERO || scaled.is_sign_negative() {
        return None;
    }
    u64::try_from(scaled.trunc()).ok()
}

// ── Market state ─────────────────────────────────────────────────────────

pub fn status_of(market: &Market) -> Option<MarketStatus> {
    MarketStatus::try_from(market.status).ok().filter(|s| *s != MarketStatus::Uninitialized)
}

/// Whether the market is still selling shares through the program (rather than trading between
/// holders on the order books, or over).
pub fn primary_open(market: &Market, now: i64) -> bool {
    status_of(market) == Some(MarketStatus::Open) && now < market.primary_end
}

/// Past the primary window and not over: shares now change hands on the Manifest order books.
pub fn secondary_open(market: &Market, now: i64) -> bool {
    match status_of(market) {
        Some(MarketStatus::Open) => now >= market.primary_end,
        Some(MarketStatus::Closed) => true,
        _ => false,
    }
}

/// What the pot pays winners if the market settled this instant: the pot minus the rake. After
/// the primary window the pot can no longer change, so for a market on the books this is final.
pub fn distributable(market: &Market) -> u64 {
    market.total_collected - rake_of(market.total_collected, market.rake_bps)
}

/// What a slice at `price` (quote atoms per slice atom, x 10^18) must win to be worth buying: its
/// price over what it pays if it wins, in basis points, capped at certainty. This is the same
/// "break-even probability" the primary market quotes, so a price on the books and a price from
/// the program mean the same thing.
pub fn secondary_breakeven_bps(market: &Market, side: usize, price: u128) -> Option<u32> {
    let pays = distributable(market) as u128;
    if pays == 0 {
        return None;
    }
    let bps = price.checked_mul(market.outcome_shares[side] as u128)?.checked_mul(10_000)? / pays.checked_mul(D18)?;
    Some(bps.min(10_000) as u32)
}

pub fn phase_of(market: &Market, now: i64) -> &'static str {
    match status_of(market) {
        Some(MarketStatus::Open) if now < market.primary_end => "primary",
        Some(MarketStatus::Open | MarketStatus::Closed) => "secondary",
        Some(MarketStatus::Settled) => "resolved",
        Some(MarketStatus::Cancelled) => "cancelled",
        // A DBC-backed market still raising its seed: not bettable yet.
        Some(MarketStatus::Pending) => "pending",
        _ => "unknown",
    }
}

fn condition_name(kind: u8) -> &'static str {
    match kind {
        condition::HOME_WIN => "home_win",
        condition::AWAY_WIN => "away_win",
        condition::DRAW => "draw",
        condition::TOTAL_GOALS_OVER => "total_goals_over",
        condition::BOTH_TEAMS_SCORE => "both_teams_score",
        condition::HOME_GOALS_OVER => "home_goals_over",
        condition::AWAY_GOALS_OVER => "away_goals_over",
        condition::HOME_WIN_BY_OVER => "home_win_by_over",
        condition::AWAY_WIN_BY_OVER => "away_win_by_over",
        _ => "unknown",
    }
}

/// Prices a bet of `deposit` atoms on `side` against the market as it stands.
pub fn quote_side(market: &Market, side: usize, deposit: u64) -> Option<BetQuote> {
    quote_bet(
        &market.outcome_shares,
        market.virtual_per_side,
        market.total_collected,
        market.fee_bps,
        market.rake_bps,
        side,
        deposit,
    )
}

fn text(metadata: &MarketMetadata, field: usize) -> String {
    metadata.field(field).and_then(|b| core::str::from_utf8(b).ok()).unwrap_or_default().to_string()
}

/// The market as the aggregator's catalog holds it. `None` for one with no text yet: without a
/// question there is nothing to show, and the market appears once its creator writes it.
///
/// `reference_deposit` is the size of the bet the displayed prices are quoted for (atoms). The
/// price of a bet depends on its size, so a single "price" needs a stated size to mean anything.
///
/// `books` are the market's order books, for a market past its primary window. A market on the
/// books with no readable, tradable book is shown with its state but never routed to.
pub fn to_venue_market(
    address: &Pubkey,
    market: &Market,
    metadata: Option<&MarketMetadata>,
    now: i64,
    reference_deposit: u64,
    books: Option<&Books>,
) -> Option<VenueMarket> {
    let metadata = metadata?;
    let status = status_of(market)?;
    let question = text(metadata, 0);
    if question.is_empty() {
        return None;
    }
    let rubric_yes = text(metadata, 1);
    let rubric_no = text(metadata, 2);
    let (label_yes, label_no) = (text(metadata, 3), text(metadata, 4));
    let category = text(metadata, 5);
    let league = text(metadata, 6);
    let id = address.to_string();

    let (yes_price, no_price) = match status {
        MarketStatus::Open if primary_open(market, now) => {
            let price = |side| {
                quote_side(market, side, reference_deposit)
                    .map(|q| Decimal::new(q.breakeven_bps as i64, 4).to_string())
            };
            (price(ID_YES), price(ID_NO))
        }
        MarketStatus::Settled => {
            let winner = market.winning_outcome as usize;
            let price = |side| Some(if side == winner { "1" } else { "0" }.to_string());
            (price(ID_YES), price(ID_NO))
        }
        // On the books: what it costs right now to buy a slice, as the odds that makes it fair.
        MarketStatus::Open | MarketStatus::Closed if secondary_open(market, now) => {
            let price = |side: usize| {
                let depth = books?[side].as_ref().filter(|d| d.tradable())?;
                let bps = secondary_breakeven_bps(market, side, depth.best_ask()?)?;
                Some(Decimal::new(bps as i64, 4).to_string())
            };
            (price(ID_YES), price(ID_NO))
        }
        _ => (None, None),
    };
    // Tradable on the books: at least one book is readable and free of orders we can't route.
    let books_tradable = books.is_some_and(|b| b.iter().flatten().any(|d| d.tradable()));

    let mut extra = serde_json::Map::new();
    extra.insert("market".into(), id.clone().into());
    extra.insert("oracle".into(), Pubkey::new_from_array(market.oracle.to_bytes()).to_string().into());
    extra.insert("creator".into(), Pubkey::new_from_array(market.creator.to_bytes()).to_string().into());
    // What each party is owed from this market's fees and rake, claimable once it settles. Shown to
    // the creator so "what have I earned" is a read, not a guess.
    extra.insert("creator_owed_usdc".into(), usdc(market.creator_owed).to_string().into());
    extra.insert("platform".into(), Pubkey::new_from_array(market.platform.to_bytes()).to_string().into());
    extra.insert("platform_owed_usdc".into(), usdc(market.platform_owed).to_string().into());
    extra.insert("condition".into(), condition_name(market.condition_kind).into());
    extra.insert("condition_param".into(), market.condition_param.into());
    extra.insert("fee_bps".into(), market.fee_bps.into());
    extra.insert("rake_bps".into(), market.rake_bps.into());
    extra.insert("price_basis_usdc".into(), usdc(reference_deposit).to_string().into());
    extra.insert(
        "price_basis".into(),
        "break-even probability of a bet of price_basis_usdc: what it must win to be fair, from what the pot actually pays".into(),
    );
    extra.insert("resolution_time_is_estimate".into(), true.into());

    let kickoff = market.bet_close_ts;
    Some(VenueMarket {
        venue_id: "metis".to_string(),
        venue_market_id: id.clone(),
        title: question.clone(),
        display_title: question.clone(),
        event: Some(VenueEvent {
            event_id: id,
            title: question,
            league: Some(league).filter(|l| !l.is_empty()),
            start_time: Some(kickoff),
            ..Default::default()
        }),
        venue_category: category.clone(),
        outcome_labels: vec![label_yes, label_no],
        market_kind: Some(condition_name(market.condition_kind).to_string()),
        rules: Some(format!(
            "YES: {rubric_yes}\nNO: {rubric_no}\n\nJudged on the 90-minute score as reported by the Metis oracle: two independent data sources must agree, and extra time and penalties never count."
        )),
        category: Category::from(category.as_str()),
        phase: phase_of(market, now).to_string(),
        start_time: kickoff,
        // Betting through the program stops at primary_end, but holders can still trade on the
        // order books until the match starts, so the market stays listed until kickoff.
        close_time: if secondary_open(market, now) { kickoff } else { market.primary_end },
        resolution_time: kickoff + EXPECTED_RESOLUTION_SECS,
        yes_price,
        no_price,
        // The pot is the market's depth: everything staked, fees included.
        liquidity: Some(usdc(market.total_collected.saturating_add(market.fees_accrued))),
        dead: matches!(status, MarketStatus::Settled | MarketStatus::Cancelled),
        // Past the primary window the market trades on Manifest's order books. If we can't read a
        // tradable book for it, it is shown with its state but never routed to.
        display_only: secondary_open(market, now) && !books_tradable,
        extra,
        ..Default::default()
    })
}

// ── Positions ────────────────────────────────────────────────────────────

/// What `shares` of `side` pay (or paid, or refund) in the market's current state, in atoms.
/// `None` when the market is in a state where that is a loss with no payout.
pub fn payout_atoms(market: &Market, side: usize, shares: u64) -> Option<u64> {
    let pool_over = |pool: u64, shares_in_pool: u64| {
        (shares_in_pool > 0).then(|| ((shares as u128 * pool as u128) / shares_in_pool as u128) as u64)
    };
    match status_of(market)? {
        MarketStatus::Open | MarketStatus::Closed => {
            // As things stand right now, if this side wins: the pot minus the rake, over the real
            // shares on that side. Later bets will move it - that is what a pari-mutuel is.
            let pot = market.total_collected;
            pool_over(pot - rake_of(pot, market.rake_bps), market.outcome_shares[side])
        }
        MarketStatus::Settled => {
            (side == market.winning_outcome as usize).then(|| pool_over(market.payout_pool, market.outcome_shares[side])).flatten()
        }
        MarketStatus::Cancelled => {
            let total: u64 = market.outcome_shares.iter().sum();
            pool_over(market.payout_pool, total)
        }
        MarketStatus::Uninitialized | MarketStatus::Pending => None,
    }
}

#[cfg(test)]
pub mod testing {
    //! Builders for markets in every state, written through the program's real types.

    use super::*;
    use metis_core::amm::{
        curve::{symmetric_seed, virtual_liquidity_per_side},
        metadata::{MARKET_METADATA_DISCRIMINATOR, QUESTION_MAX},
        state::MARKET_DISCRIMINATOR,
    };

    pub const SEED: u64 = 30_000_000;
    pub const KICKOFF: i64 = 2_000_000_000;

    pub fn market(status: MarketStatus) -> Market {
        let s = symmetric_seed(SEED, 2);
        Market {
            outcome_shares: [s, s],
            virtual_per_side: virtual_liquidity_per_side(s, 30).unwrap(),
            total_collected: SEED,
            bet_close_ts: KICKOFF,
            primary_end: KICKOFF - metis_core::constants::PRIMARY_WINDOW_SECS,
            fee_bps: 50,
            rake_bps: 100,
            dispute_secs: 900,
            discriminator: MARKET_DISCRIMINATOR,
            num_outcomes: 2,
            status: status as u8,
            winning_outcome: metis_core::constants::OUTCOME_UNRESOLVED,
            condition_kind: condition::HOME_WIN,
            ..Default::default()
        }
    }

    pub fn metadata(market: &Pubkey) -> MarketMetadata {
        let mut m: MarketMetadata = bytemuck::Zeroable::zeroed();
        m.discriminator = MARKET_METADATA_DISCRIMINATOR;
        m.market = market.to_bytes().into();
        let mut put = |i: usize, s: &str| {
            let buf: &mut [u8] = match i {
                0 => &mut m.question[..],
                1 => &mut m.rubric_yes[..],
                2 => &mut m.rubric_no[..],
                3 => &mut m.label_yes[..],
                4 => &mut m.label_no[..],
                5 => &mut m.category[..],
                _ => &mut m.league[..],
            };
            buf[..s.len()].copy_from_slice(s.as_bytes());
            m.lens[i] = s.len() as u16;
        };
        assert!("Will Team A beat Team B?".len() <= QUESTION_MAX);
        put(0, "Will Team A beat Team B?");
        put(1, "Team A leads after 90 minutes.");
        put(2, "A draw or a Team B lead after 90 minutes.");
        put(3, "Team A wins");
        put(4, "Draw or Team B");
        put(5, "Sports");
        put(6, "Example League");
        m
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    const NOW: i64 = KICKOFF - 24 * 3600;
    const REFERENCE: u64 = 10_000_000;

    fn listed(status: MarketStatus, now: i64) -> VenueMarket {
        let address = Pubkey::new_unique();
        to_venue_market(&address, &market(status), Some(&metadata(&address)), now, REFERENCE, None).unwrap()
    }

    #[test]
    fn an_open_market_shows_its_text_times_and_honest_prices() {
        let m = listed(MarketStatus::Open, NOW);
        assert_eq!(m.venue_id, "metis");
        assert_eq!(m.title, "Will Team A beat Team B?");
        assert_eq!(m.outcome_labels, ["Team A wins", "Draw or Team B"]);
        assert_eq!(m.phase, "primary");
        assert_eq!((m.start_time, m.close_time), (KICKOFF, KICKOFF - 5 * 3600));
        assert!(m.close_time < m.resolution_time, "betting closes before the market resolves");
        assert!(!m.dead && !m.display_only);
        assert_eq!(m.market_kind.as_deref(), Some("home_win"));
        assert_eq!(m.liquidity, Some(usdc(SEED)));

        // Prices are probabilities: strictly between 0 and 1.
        for price in [m.yes_price.as_deref().unwrap(), m.no_price.as_deref().unwrap()] {
            let p: Decimal = price.parse().unwrap();
            assert!(p > Decimal::ZERO && p <= Decimal::ONE, "{price}");
        }
        assert!(m.rules.clone().unwrap().contains("90-minute"));
    }

    #[test]
    fn a_market_says_what_it_owes_its_creator_and_platform() {
        let m = listed(MarketStatus::Open, NOW);
        // Every owed amount is a USDC decimal string a client can show without arithmetic on atoms.
        for field in ["creator_owed_usdc", "platform_owed_usdc"] {
            let text = m.extra.get(field).and_then(|v| v.as_str()).unwrap_or_else(|| panic!("{field} present"));
            assert!(text.parse::<Decimal>().is_ok(), "{field}: {text}");
        }
        assert!(m.extra.get("platform").and_then(|v| v.as_str()).is_some());
        assert!(m.extra.get("creator").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn the_displayed_price_is_what_a_bet_must_win_not_the_curves_probability() {
        // A balanced fresh market: the curve says 50/50 on both sides. The honest break-even for
        // a bet of this size is not 50%: the pot's payout, fees and rake decide it.
        let address = Pubkey::new_unique();
        let mk = market(MarketStatus::Open);
        let listed = to_venue_market(&address, &mk, Some(&metadata(&address)), NOW, REFERENCE, None).unwrap();
        let shown: Decimal = listed.yes_price.unwrap().parse().unwrap();
        let q = quote_side(&mk, ID_YES, REFERENCE).unwrap();
        assert_eq!(shown, Decimal::new(q.breakeven_bps as i64, 4), "the price IS the quote's break-even");
        assert_ne!(shown, Decimal::new(5000, 4), "and it is not the curve's flat 50%");
    }

    #[test]
    fn the_market_changes_phase_with_its_state_and_the_clock() {
        let primary_end = KICKOFF - 5 * 3600;
        assert_eq!(listed(MarketStatus::Open, primary_end - 1).phase, "primary");
        let after = listed(MarketStatus::Open, primary_end);
        assert_eq!(after.phase, "secondary");
        assert!(after.display_only && !after.dead, "trading moves to the books, which aren't routed to yet");
        assert!(after.yes_price.is_none(), "no program price once betting is closed");
        assert_eq!(listed(MarketStatus::Closed, NOW).phase, "secondary");
    }

    #[test]
    fn a_settled_market_shows_the_winner_at_one_and_the_loser_at_zero() {
        let address = Pubkey::new_unique();
        let mut mk = market(MarketStatus::Settled);
        mk.winning_outcome = 1;
        let m = to_venue_market(&address, &mk, Some(&metadata(&address)), KICKOFF + 99_999, REFERENCE, None).unwrap();
        assert_eq!((m.yes_price.as_deref(), m.no_price.as_deref()), (Some("0"), Some("1")));
        assert!(m.dead);
        assert_eq!(m.phase, "resolved");
    }

    #[test]
    fn a_cancelled_market_is_dead_with_no_prices() {
        let m = listed(MarketStatus::Cancelled, NOW);
        assert!(m.dead && m.yes_price.is_none() && m.no_price.is_none());
        assert_eq!(m.phase, "cancelled");
    }

    #[test]
    fn a_market_with_no_text_is_not_listed() {
        let address = Pubkey::new_unique();
        assert!(to_venue_market(&address, &market(MarketStatus::Open), None, NOW, REFERENCE, None).is_none());
    }

    #[test]
    fn a_payout_follows_the_market_through_its_life() {
        let mut mk = market(MarketStatus::Open);
        let s = mk.outcome_shares[0];
        // Open: the pot minus the rake, over the side's real shares.
        let open = payout_atoms(&mk, 0, s).unwrap();
        assert_eq!(open, ((SEED - rake_of(SEED, 100)) as u128 * s as u128 / s as u128) as u64);

        // Settled: only the winning side pays, from the recorded pool.
        mk.status = MarketStatus::Settled as u8;
        mk.winning_outcome = 0;
        mk.payout_pool = 29_700_000;
        assert_eq!(payout_atoms(&mk, 0, s), Some(29_700_000), "the whole winning side's shares take the whole pool");
        assert_eq!(payout_atoms(&mk, 1, s), None, "the losing side gets nothing");

        // Cancelled: a share is a slice of everything, whichever side.
        mk.status = MarketStatus::Cancelled as u8;
        mk.payout_pool = 30_150_000;
        let both = payout_atoms(&mk, 0, s).unwrap() + payout_atoms(&mk, 1, s).unwrap();
        assert!(both <= 30_150_000 && both >= 30_150_000 - 2);
    }

    #[test]
    fn usdc_amounts_convert_exactly_and_refuse_what_they_cannot_represent() {
        assert_eq!(to_atoms("12.5".parse().unwrap()), Some(12_500_000));
        assert_eq!(to_atoms("0.000001".parse().unwrap()), Some(1));
        assert_eq!(to_atoms("0.0000001".parse().unwrap()), None, "sub-atom precision is refused, not rounded");
        assert_eq!(to_atoms("-1".parse().unwrap()), None);
        assert_eq!(usdc(1_234_567).to_string(), "1.234567");
    }

    #[test]
    fn side_names_round_trip() {
        assert_eq!(side_index(" YES "), Some(0));
        assert_eq!(side_index("no"), Some(1));
        assert_eq!(side_index("draw"), None);
        assert_eq!(side_name(ID_NO), "no");
    }

    #[test]
    fn addresses_are_deterministic_and_distinct_per_side() {
        let program = Pubkey::new_unique();
        let market = Pubkey::new_unique();
        let (yes, no) = (outcome_mint(&program, &market, 0), outcome_mint(&program, &market, 1));
        assert_ne!(yes, no);
        assert_eq!(yes, outcome_mint(&program, &market, 0));
        let owner = Pubkey::new_unique();
        assert_ne!(associated_token_account(&owner, &yes), associated_token_account(&owner, &no));
    }
}
