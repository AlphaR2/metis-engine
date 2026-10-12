//! The honest quote: what a bettor would actually receive, not what the curve's price implies.
//!
//! The curve (with its virtual liquidity) decides how many shares a deposit buys. What those
//! shares are WORTH if they win is a different number, set only by the real pot and the real
//! shares: `(pot - rake) / winning shares`. Quoting the second is what keeps the displayed odds
//! honest - the aggregator and any UI should show these, never the curve's own probability.
//!
//! Pure integer math over the same functions `place_bet` runs, so a quote taken against current
//! state is exactly what the instruction will do (if nobody else bets in between).

use crate::{
    amm::curve::{priced_shares, quote_buy},
    constants::{BPS_DENOMINATOR, MAX_OUTCOMES},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BetQuote {
    /// What leaves the bettor's wallet.
    pub deposit: u64,
    /// The share of it taken as a fee before the curve sees it.
    pub fee: u64,
    /// What actually enters the pot.
    pub net: u64,
    pub shares: u64,
    /// What those shares pay if this side wins, assuming nobody bets after this. After the rake.
    pub payout_if_win: u64,
    /// The probability at which this bet breaks even, in basis points: deposit / payout_if_win.
    /// Capped at 10_000 (a bet that pays back less than it cost needs certainty to break even).
    pub breakeven_bps: u32,
    /// How far this bet moves the curve's own probability, in basis points. Not the odds - the
    /// size of the move, which is what the on-chain price-impact ceiling limits.
    pub curve_impact_bps: u32,
}

/// The fee on `deposit` at `fee_bps`, rounded down.
pub fn fee_of(deposit: u64, fee_bps: u16) -> u64 {
    ((deposit as u128 * fee_bps as u128) / BPS_DENOMINATOR as u128) as u64
}

/// The rake on `pot` at `rake_bps`, rounded down.
pub fn rake_of(pot: u64, rake_bps: u16) -> u64 {
    ((pot as u128 * rake_bps as u128) / BPS_DENOMINATOR as u128) as u64
}

/// Quotes a bet. `None` if it would be rejected for being too small or overflowing.
#[allow(clippy::too_many_arguments)]
pub fn quote_bet(
    outcome_shares: &[u64; MAX_OUTCOMES],
    virtual_per_side: u64,
    total_collected: u64,
    fee_bps: u16,
    rake_bps: u16,
    outcome_index: usize,
    deposit: u64,
) -> Option<BetQuote> {
    let fee = fee_of(deposit, fee_bps);
    let net = deposit.checked_sub(fee)?;
    let priced = priced_shares(outcome_shares, virtual_per_side)?;
    let curve = quote_buy(&priced, outcome_index, net)?;

    let pot_after = total_collected.checked_add(net)?;
    let winning_after = outcome_shares[outcome_index].checked_add(curve.shares_received)?;
    let distributable = pot_after - rake_of(pot_after, rake_bps);
    let payout_if_win =
        ((curve.shares_received as u128 * distributable as u128) / winning_after as u128) as u64;

    let breakeven_bps = if payout_if_win == 0 {
        BPS_DENOMINATOR as u32
    } else {
        ((deposit as u128 * BPS_DENOMINATOR as u128) / payout_if_win as u128).min(BPS_DENOMINATOR as u128) as u32
    };

    Some(BetQuote {
        deposit,
        fee,
        net,
        shares: curve.shares_received,
        payout_if_win,
        breakeven_bps,
        curve_impact_bps: curve.price_impact_bps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amm::curve::{symmetric_seed, virtual_liquidity_per_side};

    const SEED: u64 = 30_000_000;

    fn fresh(k: u64) -> ([u64; 2], u64) {
        let s = symmetric_seed(SEED, 2);
        ([s, s], virtual_liquidity_per_side(s, k).unwrap())
    }

    // The worked example from the design discussion: a $20 seed, k=30, Alice puts $40 on A. The
    // curve calls A a coin flip, but the real pot only pays about 1.19x.
    #[test]
    fn the_honest_quote_disagrees_with_the_curve_exactly_as_the_design_notes_say() {
        let s = symmetric_seed(20_000_000, 2);
        let shares = [s, s];
        let v = virtual_liquidity_per_side(s, 30).unwrap();
        let q = quote_bet(&shares, v, 20_000_000, 0, 0, 0, 40_000_000).unwrap();

        let multiple = q.payout_if_win as f64 / q.deposit as f64;
        println!("curve said 50%; honest quote: wins {} ({multiple:.2}x), breakeven {}bps", q.payout_if_win, q.breakeven_bps);
        assert!((1.15..1.25).contains(&multiple), "about 1.19x, got {multiple}");
        assert!(q.breakeven_bps > 8_000, "needs A to be ~84% likely, got {}", q.breakeven_bps);
    }

    #[test]
    fn fee_comes_off_before_the_curve_and_the_net_is_what_enters_the_pot() {
        let (shares, v) = fresh(30);
        let q = quote_bet(&shares, v, SEED, 50, 0, 0, 10_000_000).unwrap();
        assert_eq!(q.fee, 50_000, "0.5% of $10");
        assert_eq!(q.net, 9_950_000);
        assert_eq!(q.fee + q.net, q.deposit);
    }

    #[test]
    fn the_rake_comes_out_of_the_payout() {
        let (shares, v) = fresh(30);
        let free = quote_bet(&shares, v, SEED, 0, 0, 0, 10_000_000).unwrap();
        let raked = quote_bet(&shares, v, SEED, 0, 300, 0, 10_000_000).unwrap();
        assert!(raked.payout_if_win < free.payout_if_win);
        let ratio = raked.payout_if_win as f64 / free.payout_if_win as f64;
        assert!((ratio - 0.97).abs() < 0.001, "a 3% rake takes ~3% of the payout, got {ratio}");
    }

    #[test]
    fn rounding_never_favours_the_bettor() {
        assert_eq!(fee_of(1, 50), 0);
        assert_eq!(fee_of(199, 50), 0, "199 * 50 / 10000 floors to 0");
        assert_eq!(rake_of(199, 100), 1);
        let (shares, v) = fresh(30);
        let q = quote_bet(&shares, v, SEED, 50, 100, 0, 1_000_001).unwrap();
        assert_eq!(q.fee + q.net, q.deposit);
    }

    #[test]
    fn a_dust_deposit_that_buys_nothing_is_refused() {
        let (shares, v) = fresh(0);
        assert!(quote_bet(&shares, v, SEED, 0, 0, 0, 0).is_none());
    }

    // The invariant that makes the quote trustworthy: when nobody bets after you, the amount the
    // quote promised is exactly what claim pays.
    #[test]
    fn the_promised_payout_matches_what_a_claim_would_pay() {
        use crate::settlement::payout::compute_payout;
        let (shares, v) = fresh(30);
        let q = quote_bet(&shares, v, SEED, 50, 100, 1, 25_000_000).unwrap();

        let mut after = shares;
        after[1] += q.shares;
        let pot = SEED + q.net;
        let pool = pot - rake_of(pot, 100);
        let paid = compute_payout(q.shares, pool, after[1]);
        assert_eq!(paid, q.payout_if_win);
    }
}
