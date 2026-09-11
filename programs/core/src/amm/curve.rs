/// MARKER --  Most important file in this entire codebase. The curve is the heart of the AMM, and this is the only place it is implemented. Every other file is just plumbing around this one formula. 

// The market price formula. Cost of the pool = sqrt(sum of each side's
// shares, squared). Everything below is built on this one function.

use crate::constants::MAX_OUTCOMES;

/// Square root of a whole number, rounded down. Uses u128 so squaring a
/// u64 never overflows.
pub fn isqrt(n: u128) -> u128 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = x.div_ceil(2);

    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// Cost of the pool right now: sqrt(sum of each side squared).
pub fn cost(outcome_shares: &[u64; MAX_OUTCOMES]) -> u128 {
    let sum_sq: u128 = outcome_shares
        .iter()
        .map(|&q| (q as u128) * (q as u128))
        .sum();
    isqrt(sum_sq)
}

/// The creator's fee, split evenly across every outcome so the market
/// opens balanced. F=$10, 2 outcomes -> about 7 shares each side.
pub fn symmetric_seed(fee: u64, num_outcomes: u8) -> u64 {
    if num_outcomes < 2 {
        return 0;
    }
    let fee_sq = (fee as u128) * (fee as u128);
    let n = num_outcomes as u128;
    isqrt(fee_sq / n) as u64
}

    /// docs/mvp.md 1.4: virtual_per_side = k × (seed / 2^(1/r)). At r = 2,
    /// seed / 2^(1/r) = seed/√2 — exactly the quantity `symmetric_seed` already
    /// computes (floor-rounded). So this is just that output, scaled by k.
    pub fn virtual_liquidity_per_side(seed_per_side: u64, k: u64) -> Option<u64> {
    seed_per_side.checked_mul(k)
    }

/// The array every curve function should actually be called with once
/// virtual liquidity exists: real shares plus the market's fixed virtual
/// offset on every side. 
/// 
/// This is simply `[qa + v, qb + v]` where `virtual_per_side` is added to every side's real shares. The curve is still continuous and nonlinear, but the offset makes it deeper and less sensitive to small trades.
pub fn priced_shares(
    outcome_shares: &[u64; MAX_OUTCOMES],
    virtual_per_side: u64,
) -> Option<[u64; MAX_OUTCOMES]> {
    let mut out = [0u64; MAX_OUTCOMES];
    for (o, &q) in out.iter_mut().zip(outcome_shares.iter()) {
        *o = q.checked_add(virtual_per_side)?;
    }
    Some(out)
}
/// Buying shares with a payment. Returns the NEW total for that outcome,
/// not how many shares were bought — the caller subtracts the old total
/// to get that.
///
/// Formula: new_qi = sqrt((payment + cost_before)^2 - sum of the other sides squared)
///
/// Returns None if the numbers don't add up, which should never happen
/// unless something upstream is already broken.
pub fn buy_shares(
    outcome_shares: &[u64; MAX_OUTCOMES],
    outcome_index: usize,
    payment: u64,
) -> Option<u64> {
    let c_old = cost(outcome_shares);
    let target = (payment as u128) + c_old;
    let target_sq = target * target;

    let others_sq: u128 = outcome_shares
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != outcome_index)
        .map(|(_, &q)| (q as u128) * (q as u128))
        .sum();

    let diff = target_sq.checked_sub(others_sq)?;
    Some(isqrt(diff) as u64)
}

/// The price a bettor sees and trades against right now. Pricing only, not
/// payouts — payouts only ever read outcome_shares directly.
///
pub fn price(outcome_shares: &[u64; MAX_OUTCOMES], outcome_index: usize) -> f64 {
    let denom = (cost(outcome_shares) as f64).max(1.0);
    outcome_shares[outcome_index] as f64 / denom
}

/// Fixed-point scale for `effective_execution_price_scaled`: that field is
/// `deposit_amount * PRICE_SCALE / shares_received`, i.e. quote-units per
/// share with `PRICE_SCALE`'s number of implied decimal places. Divide by
/// this (as a float, off-chain) only for display — nothing on-chain needs
/// to.
pub const PRICE_SCALE: u128 = 1_000_000_000;

/// Basis-point scale (1 bps = 0.01%) used for every probability-shaped
/// field on `BuyQuote`. `10_000` bps = 100%.
pub const BPS_SCALE: u128 = 10_000;

/// A single finite transaction, quoted as one effective execution price
/// rather than the instantaneous marginal price. The curve underneath is
/// still continuous and nonlinear — this is a reporting view over the exact
/// same transition `buy_shares()`/`cost()` already compute, not a second
/// pricing implementation and not an approximation via many smaller trades.
///
/// Every field is an exact integer — no `f64` anywhere in this struct or in
/// `quote_buy()` below. Probabilities are basis points (`BPS_SCALE` = 100%);
/// the execution price is fixed-point, scaled by `PRICE_SCALE`. A caller
/// that wants a human-readable percentage or decimal divides by the
/// relevant scale itself, off-chain, in whatever precision it needs — that
/// conversion never has to happen inside the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuyQuote {
    pub deposit_amount: u64,
    pub shares_received: u64,
    /// `deposit_amount * PRICE_SCALE / shares_received` — quote-units per
    /// share, scaled by `PRICE_SCALE`.
    pub effective_execution_price_scaled: u128,
    /// Basis points, `0..=BPS_SCALE`.
    pub probability_before_bps: u32,
    /// Basis points, `0..=BPS_SCALE`.
    pub probability_after_bps: u32,
    /// Signed basis points: `probability_after_bps - probability_before_bps`.
    pub probability_change_bps: i32,
    /// `|probability_change_bps|`. Called Price Impact (or Probability
    /// Impact) throughout this codebase — never "slippage", which is a
    /// related but distinct concept this field does not measure.
    pub price_impact_bps: u32,
    pub current_curve_cost: u128,
    pub new_curve_cost: u128,
}

/// `qi^2 * BPS_SCALE / cost^2` as basis points — the same probability
/// definition `price(q, i)^2` computes (production's curvature is r = 2, so
/// `P_i = qi^2 / cost^2`), derived directly in integer arithmetic instead of
/// through `price()`'s `f64` return. Returns `None` on overflow (share
/// counts and costs realistic markets never approach) rather than wrapping
/// or panicking, matching every other arithmetic step in this module.
fn probability_bps(
    outcome_shares: &[u64; MAX_OUTCOMES],
    outcome_index: usize,
    curve_cost: u128,
) -> Option<u32> {
    if curve_cost == 0 {
        return Some(0);
    }
    let qi = outcome_shares[outcome_index] as u128;
    let numerator = qi.checked_mul(qi)?.checked_mul(BPS_SCALE)?;
    let denominator = curve_cost.checked_mul(curve_cost)?;
    Some((numerator / denominator) as u32)
}

/// Quotes a buy without mutating anything: given the market's current share
/// state, what would `deposit_amount` into `outcome_index` actually do?
///
/// This calls `buy_shares()` and `cost()` — the same functions
/// `process_place_bet` executes with — rather than re-deriving the curve
/// transition. The only new work here is packaging the before/after state
/// into one reporting struct, including the transaction's single effective
/// execution price `D / ΔA` (see the module docs for the derivation: this
/// equals the average of the marginal price along the whole trade path,
/// without ever needing to integrate that path numerically).
///
/// Returns `None` if the underlying `buy_shares()` call fails, the deposit
/// mints zero shares (rounding can floor a tiny deposit against a deep pool
/// to zero, and an execution price needs a nonzero denominator — the same
/// case `process_place_bet` already rejects with `PaymentTooSmall`), or any
/// intermediate multiplication overflows `u128`.
pub fn quote_buy(
    outcome_shares: &[u64; MAX_OUTCOMES],
    outcome_index: usize,
    deposit_amount: u64,
) -> Option<BuyQuote> {
    let cost_before = cost(outcome_shares);
    let old_total = outcome_shares[outcome_index];

    let new_total = buy_shares(outcome_shares, outcome_index, deposit_amount)?;
    let shares_received = new_total.checked_sub(old_total)?;
    if shares_received == 0 {
        return None;
    }

    let mut new_shares = *outcome_shares;
    new_shares[outcome_index] = new_total;
    let cost_after = cost(&new_shares);

    let probability_before_bps = probability_bps(outcome_shares, outcome_index, cost_before)?;
    let probability_after_bps = probability_bps(&new_shares, outcome_index, cost_after)?;
    let probability_change_bps = probability_after_bps as i32 - probability_before_bps as i32;

    let effective_execution_price_scaled = (deposit_amount as u128)
        .checked_mul(PRICE_SCALE)?
        .checked_div(shares_received as u128)?;

    Some(BuyQuote {
        deposit_amount,
        shares_received,
        effective_execution_price_scaled,
        probability_before_bps,
        probability_after_bps,
        probability_change_bps,
        price_impact_bps: probability_change_bps.unsigned_abs(),
        current_curve_cost: cost_before,
        new_curve_cost: cost_after,
    })
}

// Tests for the pricing curve. To see the printed output:
//   cargo test -p metis-core --lib amm::curve -- --nocapture --test-threads=1
//
// Every test starts with its inputs as named `let` bindings marked "dial".
// Change them to explore edge cases - the assertions are written as properties
// that must hold for ANY value, not as hardcoded answers, so turning a dial
// shouldn't break a test unless it exposes a real problem.

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // This is the test to make sure our square root formula is working.
    // isqrt is the only square root available on-chain, and every price is
    // built on it. For exact squares it must land exactly, with no drift.
    #[test]
    fn isqrt_perfect_squares() {
        let roots = [0u128, 1, 6, 8, 10, 100, 200]; // dial: any roots you like

        for r in roots {
            let n = r * r;
            let got = isqrt(n);
            println!("isqrt({n}) = {got}   (expected {r})");
            assert_eq!(got, r, "{n} is a perfect square, so the root must be exact");
        }
    }

    // Test to make sure we avoid rounding maths in the square root formula.
    // It rounds DOWN, never up. Rounding up anywhere in the curve would let a
    // market mint shares nobody paid for.
    #[test]
    fn isqrt_floors_non_squares() {
        let values = [98u128, 99, 101, 999, 123_456]; // dial

        for n in values {
            let root = isqrt(n);
            let true_root = (n as f64).sqrt();
            println!("isqrt({n}) = {root}   (true root {true_root:.4}, so it floored)");
            assert_eq!(
                root,
                true_root.floor() as u128,
                "isqrt should equal the floor of the real square root"
            );
        }
    }

    // Test to also confirm we do not round up. We round down.
    // The floor property stated properly: root^2 never exceeds n, and one more
    // than the root always does. That's what "rounds down" actually means,
    // checked across the range rather than at hand-picked points.
    #[test]
    fn isqrt_is_always_the_true_floor() {
        // dial: add anything, including extremes
        let values = [0u128, 1, 2, 3, 8, 15, 16, 17, 1_000_000, u64::MAX as u128];

        for n in values {
            let root = isqrt(n);
            println!("isqrt({n}) = {root}");
            assert!(root * root <= n, "root^2 overshot n for n={n}");
            assert!(
                (root + 1).checked_mul(root + 1).is_none_or(|sq| sq > n),
                "root was too small for n={n} - (root+1)^2 should exceed n"
            );
        }
    }

    // The test to check that the seed is split properly. It divides the
    // creator's stake evenly so the market opens with no favourite.
    //
    // The assertions are properties rather than a fixed number, so any fee and
    // outcome count can be dialled in: every side gets the same amount, the
    // total never exceeds what was paid, and it always rounds down.
    #[test]
    fn symmetric_seed_splits_the_stake_evenly() {
        let fee = 50u64; // dial
        let outcomes = 2u8; // dial (2 or more)

        let seed = symmetric_seed(fee, outcomes);
        let total_cost = isqrt((seed as u128) * (seed as u128) * outcomes as u128);

        println!("symmetric_seed(fee {fee}, outcomes {outcomes}) = {seed} shares per side");
        println!("  that pool measures {total_cost}, against a fee of {fee}");
        println!("  lost to rounding: {}", fee as u128 - total_cost);

        assert!(
            seed > 0,
            "a real fee must seed real shares - is the fee too small?"
        );
        assert!(
            total_cost <= fee as u128,
            "the seeded pool must never measure more than the fee that paid for it"
        );
    }

    // A market needs at least two outcomes. One would put the whole stake on a
    // single side - a guaranteed winner with nothing to price - and zero used
    // to divide by zero and panic outright. Both now return 0, which callers
    // must treat as a refusal.
    //
    // initialize_market rejects these before it ever gets here, so this is
    // testing the backstop rather than the live path.
    #[test]
    fn symmetric_seed_refuses_fewer_than_two_outcomes() {
        let fee = 500u64; // dial

        for outcomes in [0u8, 1] {
            let seed = symmetric_seed(fee, outcomes);
            println!(
                "symmetric_seed(fee {fee}, outcomes {outcomes}) = {seed}   (refused, no panic)"
            );
            assert_eq!(
                seed, 0,
                "fewer than two outcomes must return 0, not a real seed"
            );
        }

        // Two is the first count that produces a usable market.
        let ok = symmetric_seed(fee, 2);
        println!("symmetric_seed(fee {fee}, outcomes 2) = {ok}   (accepted)");
        assert!(ok > 0);
    }

    // Rounding down happens twice - once seeding, once measuring the pool - so
    // the opening cost lands just under the fee that paid for it. Expected, and
    // always in the pool's favour rather than the creator's.
    #[test]
    fn cost_of_symmetric_seed_never_exceeds_the_fee() {
        let fee = 10u64; // dial
        let outcomes = 2u8; // dial

        let q0 = symmetric_seed(fee, outcomes);
        let mut pool = [0u64; MAX_OUTCOMES];
        for slot in pool.iter_mut().take(outcomes as usize) {
            *slot = q0;
        }
        let c = cost(&pool);

        println!("fee {fee} across {outcomes} outcomes -> {q0} shares per side");
        println!("  pool {pool:?} measures at cost {c}");
        println!(
            "  {} lost to rounding (always in the pool's favour)",
            fee as u128 - c
        );

        assert!(
            c <= fee as u128,
            "the pool must never measure more than was paid in - that would be minting money"
        );
    }

    // cost() is the function everything else stands on: the length of the
    // share vector. Worth checking directly, not just through its callers.
    #[test]
    fn cost_is_the_length_of_the_share_vector() {
        let pools = [[0u64, 0], [3, 4], [7, 7], [1_000_000, 0]]; // dial

        for pool in pools {
            let c = cost(&pool);
            let exact = ((pool[0] as f64).powi(2) + (pool[1] as f64).powi(2)).sqrt();
            println!("cost({pool:?}) = {c}   (exact {exact:.4})");
            assert!(c as f64 <= exact + 0.0001, "cost must never round up");
        }

        // 3-4-5 triangle - the one case where the answer is a whole number.
        assert_eq!(cost(&[3, 4]), 5);
    }

    // The test for slippage demonstration. A naive "the price won't move" guess always
    // overestimates what you get, because the price moves against you as you
    // buy. That gap IS the slippage, and it's what makes shifting the price
    // expensive rather than free.
    #[test]
    fn buying_always_gets_less_than_a_flat_price_would_suggest() {
        let seed_fee = 10u64; // dial: what the creator staked
        let bet = 13u64; // dial: what the bettor pays
        let side = 0usize; // dial: which outcome

        // 1. creator seeds the market
        let q0 = symmetric_seed(seed_fee, 2);
        let pool = [q0, q0];
        println!("creator staked {seed_fee} -> pool {pool:?}");

        // 2. someone buys in
        let new_total = buy_shares(&pool, side, bet).expect("no underflow");
        let received = new_total - pool[side];
        let naive = bet as f64 / price(&pool, side);

        println!("bettor 1, paying {bet} into side {side}:");
        println!(" expected/naive shares estimate: {naive:.2} shares");
        println!("  actually got:   {received} shares");
        println!(
            "  slippage:       {:.1}%",
            (naive - received as f64) / naive * 100.0
        );

        assert!(
            (received as f64) < naive,
            "the curve must always deliver less than a flat price would - \
             got {received}, flat-price estimate was {naive:.2}"
        );
    }

    // THE test to actually see why slippage happens. Same pool, escalating bet
    // sizes.
    //
    // IMPORTANT: this only shows a clean monotonic curve at REALISTIC scale.
    // Every real market seeds with at least MIN_SEED = 20_000_000 (millionths),
    // not a toy number like 100. At tiny integer scale isqrt's floor-rounding
    // isn't a small correction any more, it's the dominant effect, and the
    // "bigger bet slips more" property gets swamped by rounding noise - try
    // seed_fee = 100 with these same bets and watch slippage jump around
    // instead of climbing smoothly. That's not a bug in the curve; it's what
    // "the maths only holds at scale" actually looks like when shrunk too far.
    #[test]
    fn slippage_tracks_bet_size_against_pool_depth() {
        let seed_fee = 100_000_000u64; // dial: pool depth, MIN_SEED scale
        // dial: bet sizes as fractions of seed_fee, IN ANY ORDER - the table
        // below sorts by bet size before printing/checking, so you don't have
        // to type these ascending yourself to explore.
        let bets: Vec<u64> = [0.80, 0.05, 0.43, 0.25, 0.50, 0.05]
            .iter()
            .map(|frac| (seed_fee as f64 * frac) as u64)
            .collect();

        let q0 = symmetric_seed(seed_fee, 2);
        let pool = [q0, q0];
        let flat_price = price(&pool, 0);

        // Compute one row per bet, in the order you dialled them above - the
        // table below prints in THIS order, exactly as typed.
        let rows: Vec<(u64, f64, f64, u64, f64)> = bets
            .iter()
            .map(|&bet| {
                let new_total = buy_shares(&pool, 0, bet).expect("no underflow");
                let received = new_total - pool[0];
                let naive = bet as f64 / flat_price;
                let slippage = (naive - received as f64) / naive * 100.0;
                let pct_of_pool = bet as f64 / q0 as f64 * 100.0;
                (bet, pct_of_pool, naive, received, slippage)
            })
            .collect();

        println!();
        println!("pool seeded with {seed_fee} -> {pool:?}  (marginal price {flat_price:.4})");
        println!();
        println!(
            "{:>8} {:>10} {:>12} {:>10} {:>10}",
            "bet", "% of pool", "naive est.", "actual", "slippage"
        );
        println!("{}", "-".repeat(56));
        for (bet, pct_of_pool, naive, received, slippage) in &rows {
            println!("{bet:>8} {pct_of_pool:>9.1}% {naive:>12.2} {received:>10} {slippage:>9.1}%");
        }
        println!();
        println!("the pattern: slippage grows with bet-size-relative-to-pool-depth,");
        println!("not with the bet's absolute dollar amount - look at which rows have");
        println!("the bigger 'bet' number, not which row comes first in the table.");
        println!();

        // The property check is done on a SEPARATE sorted copy, so it's
        // testing "bigger bet slips more" against real size, not against the
        // order you happened to type things in above.
        let mut by_size = rows.clone();
        by_size.sort_by_key(|(bet, ..)| *bet);
        for pair in by_size.windows(2) {
            let (smaller_bet, _, _, _, smaller_slippage) = pair[0];
            let (bigger_bet, _, _, _, bigger_slippage) = pair[1];
            assert!(
                bigger_slippage >= smaller_slippage,
                "a bigger bet against the same pool must never slip LESS than a smaller one \
                 (bet={bigger_bet} slipped {bigger_slippage:.1}%, bet={smaller_bet} slipped {smaller_slippage:.1}%)"
            );
        }
    }

    // Real deposits must never blow up the maths, whatever the pool looks like.
    #[test]
    fn buy_shares_never_underflows_on_a_healthy_market() {
        let pool = [7u64, 7u64]; // dial
        let payments = [1u64, 5, 50, 1_000]; // dial

        for payment in payments {
            let a = buy_shares(&pool, 0, payment);
            let b = buy_shares(&pool, 1, payment);
            println!("pool {pool:?}, paying {payment}: side0 -> {a:?}, side1 -> {b:?}");
            assert!(a.is_some() && b.is_some());
        }
    }

    // Paying more can never buy fewer shares, across a spread of pool shapes.
    //
    // Floor rounding means a tiny payment against a huge pool can legitimately
    // mint zero, so this checks the ordering rather than "always > 0".
    #[test]
    fn buy_shares_never_underflows_and_is_monotonic_in_payment() {
        let pools = [[7u64, 7u64], [100, 50], [1, 1_000_000], [500_000, 500_000]]; // dial
        let payments = [1u64, 10, 12_345, 1_000_000]; // dial (must be ascending)

        for pool in pools {
            for side in 0..2usize {
                let mut last_delta = 0u64;
                print!("pool {pool:?} side {side}:");
                for &payment in &payments {
                    let new_total = buy_shares(&pool, side, payment)
                        .expect("healthy pool + payment should never overflow/underflow");
                    let delta = new_total - pool[side];
                    print!("  pay {payment} -> {delta} shares");
                    assert!(
                        delta >= last_delta,
                        "a larger payment must never buy fewer shares \
                         (pool={pool:?}, side={side}, payment={payment}, delta={delta}, previous={last_delta})"
                    );
                    last_delta = delta;
                }
                println!();
            }
        }
    }

    // Prices here are normalised coordinates, NOT probabilities - they don't
    // sum to 1. Their squares do, and that's the number worth showing a user.
    //
    // Integer rounding in the denominator means the sum isn't exactly 1 at tiny
    // pool sizes. This quantifies that drift and shows it vanishes at the scale
    // real markets actually run at.
    #[test]
    fn price_squared_sums_to_probability_one_and_drift_shrinks_at_scale() {
        let tiny = [7u64, 7u64]; // dial: a deliberately tiny pool
        let real = [7_071_067u64, 7_071_067u64]; // dial: a realistic one

        let tiny_sum = price(&tiny, 0).powi(2) + price(&tiny, 1).powi(2);
        let real_sum = price(&real, 0).powi(2) + price(&real, 1).powi(2);

        println!("tiny pool {tiny:?}");
        println!(
            "  prices:  {:.4} + {:.4} = {:.4}  <- NOT 1, these aren't probabilities",
            price(&tiny, 0),
            price(&tiny, 1),
            price(&tiny, 0) + price(&tiny, 1)
        );
        println!(
            "  squared: {tiny_sum:.6}  (drift from 1: {:.6})",
            (tiny_sum - 1.0).abs()
        );
        println!("real-scale pool {real:?}");
        println!(
            "  squared: {real_sum:.6}  (drift from 1: {:.6})",
            (real_sum - 1.0).abs()
        );

        let tiny_drift = (tiny_sum - 1.0).abs();
        let real_drift = (real_sum - 1.0).abs();

        assert!(
            real_drift <= tiny_drift,
            "expected drift to shrink at real scale"
        );
        assert!(
            real_drift < 0.001,
            "real-scale drift {real_drift:.6} is larger than expected"
        );
    }

    // The settlement multiplier. If an outcome wins, each of its shares is
    // worth 1/price - so price and payout-per-share always multiply to 1.
    //
    // This is what ties the trading curve to the pari-mutuel payout: the pool
    // ends up equal to the curve's own cost, so splitting it among the winning
    // shares gives exactly the inverse of their price.
    #[test]
    fn payout_per_share_is_the_inverse_of_price() {
        let pools = [[7_071_067u64, 7_071_067], [400, 916], [1_000_000, 250_000]]; // dial

        for pool in pools {
            let total = cost(&pool) as f64;
            for side in 0..2usize {
                let p = price(&pool, side);
                let payout_per_share = total / pool[side] as f64;
                println!(
                    "pool {pool:?} side {side}: price {p:.4} x payout/share {payout_per_share:.4} = {:.4}",
                    p * payout_per_share
                );
                assert!(
                    (p * payout_per_share - 1.0).abs() < 0.001,
                    "price x payout-per-share should be 1"
                );
            }
        }
    }

    // ── quote_buy(): single-transaction execution quoting ──────────────

    // Test A - basic quote. A known pool and deposit should produce shares,
    // an execution price, and before/after probabilities that all agree
    // with independently computing them via buy_shares()/cost()/price() -
    // quote_buy() is a packaging convenience, not a second implementation,
    // so it must never disagree with the functions it wraps.
    #[test]
    fn quote_buy_basic() {
        let pool = [7_071_067u64, 7_071_067]; // dial: a realistic $10-scale pool
        let deposit = 1_000_000u64; // dial

        let quote = quote_buy(&pool, 0, deposit).expect("valid quote");

        let expected_new_total = buy_shares(&pool, 0, deposit).expect("valid buy");
        let expected_shares = expected_new_total - pool[0];
        let expected_p_before_bps =
            probability_bps(&pool, 0, cost(&pool)).expect("no overflow at this scale");
        let expected_p_after_bps = {
            let mut after = pool;
            after[0] = expected_new_total;
            probability_bps(&after, 0, cost(&after)).expect("no overflow at this scale")
        };

        println!(
            "deposit {deposit} into {pool:?}: {} shares, exec price (scaled) {}, \
             P {} -> {} bps",
            quote.shares_received,
            quote.effective_execution_price_scaled,
            quote.probability_before_bps,
            quote.probability_after_bps
        );

        assert_eq!(quote.deposit_amount, deposit);
        assert_eq!(quote.shares_received, expected_shares);
        assert_eq!(quote.probability_before_bps, expected_p_before_bps);
        assert_eq!(quote.probability_after_bps, expected_p_after_bps);
        assert_eq!(
            quote.effective_execution_price_scaled,
            (deposit as u128) * PRICE_SCALE / (expected_shares as u128)
        );
        assert!(quote.probability_after_bps > quote.probability_before_bps);
    }

    // Test B - integral consistency (research validation only; this is a
    // #[cfg(test)]-only numerical integration, never part of the production
    // path). The module docs claim D = integral of the marginal price along
    // the trade path. Approximating that integral with a fine Riemann sum
    // over price() and comparing it to the actual cost delta checks that
    // claim against the real curve, without ever using integration to
    // EXECUTE a trade.
    #[test]
    fn quote_buy_integral_consistency() {
        let pool = [7_071_067u64, 7_071_067]; // dial
        let deposit = 2_000_000u64; // dial

        let quote = quote_buy(&pool, 0, deposit).expect("valid quote");
        let actual_cost_delta = (quote.new_curve_cost - quote.current_curve_cost) as f64;

        let a_start = pool[0] as f64;
        let a_end = (pool[0] + quote.shares_received) as f64;
        let b = pool[1] as f64;
        let steps = 200_000u32; // dial: finer -> closer to the true integral
        let dx = (a_end - a_start) / steps as f64;

        // Midpoint rule over p(x) = x / sqrt(x^2 + b^2), the continuous
        // marginal price - this is the research-only numerical
        // approximation the module docs explicitly forbid using on-chain.
        let mut integral = 0.0f64;
        for i in 0..steps {
            let x = a_start + (i as f64 + 0.5) * dx;
            let p = x / (x * x + b * b).sqrt();
            integral += p * dx;
        }

        println!(
            "cost delta {actual_cost_delta:.4} vs integral-of-price {integral:.4} \
             (deposit was {deposit})"
        );
        assert!(
            (integral - actual_cost_delta).abs() < 1.0,
            "integral of marginal price should match the actual cost delta \
             within numerical tolerance: {integral} vs {actual_cost_delta}"
        );
    }

    // Test C - small trade. For a deposit tiny relative to the pool, the
    // effective execution price should sit close to the pre-trade marginal
    // price (in the same q-per-cost units `price()` returns), converging
    // as the trade shrinks - exactly what the calculus in the module docs
    // predicts for an infinitesimal trade.
    #[test]
    fn quote_buy_small_trade_approaches_marginal_price() {
        let pool = [7_071_067u64, 7_071_067]; // dial: realistic scale so rounding is negligible
        // Small relative to the pool (about 1.4% of it) but still large
        // enough in absolute integer terms that isqrt rounding noise
        // doesn't dominate - a literal deposit of 10 against this pool hits
        // the same "toy regime" integer-rounding effect documented in the
        // research crate's Experiment D, which would make this test measure
        // rounding noise instead of the calculus it's meant to check.
        let tiny_deposit = 100_000u64; // dial

        let quote = quote_buy(&pool, 0, tiny_deposit).expect("valid quote");

        // price() already returns dC/dqi in quote-units per share - the
        // same units as the execution price - so the two are directly
        // comparable once the scaled integer is converted back to a plain
        // ratio. That conversion is test-only (println!/assert! reporting),
        // never something quote_buy() itself does.
        let marginal = price(&pool, 0);
        let exec = quote.effective_execution_price_scaled as f64 / PRICE_SCALE as f64;

        println!(
            "tiny deposit {tiny_deposit}: marginal price {marginal:.6}, execution price {exec:.6}"
        );
        assert!(
            (exec - marginal).abs() < 0.05,
            "a small trade's execution price should sit close to the marginal price"
        );
    }

    // Test D - large trade. For a deposit that materially moves the market,
    // the execution price must differ from BOTH the starting and ending
    // marginal price - proof that a finite order cannot be treated as
    // executing entirely at either endpoint's price.
    #[test]
    fn quote_buy_large_trade_diverges_from_both_endpoints() {
        let pool = [7_071_067u64, 7_071_067]; // dial
        let large_deposit = 5_000_000u64; // dial: half the pool's cost

        let quote = quote_buy(&pool, 0, large_deposit).expect("valid quote");
        let marginal_before = price(&pool, 0);
        let mut after = pool;
        after[0] += quote.shares_received;
        let marginal_after = price(&after, 0);

        // Test-only conversion back to a plain ratio, purely for comparison
        // against price()'s f64 return - quote_buy() itself never does this.
        let exec = quote.effective_execution_price_scaled as f64 / PRICE_SCALE as f64;

        println!(
            "large deposit {large_deposit}: marginal before {marginal_before:.4}, \
             execution {exec:.4}, marginal after {marginal_after:.4}"
        );
        assert!(
            exec > marginal_before,
            "execution price should exceed the starting marginal price for a large buy"
        );
        assert!(
            exec < marginal_after,
            "execution price should stay under the ending marginal price for a large buy"
        );
    }

    // Test E - same economics. quote_buy() must never produce a different
    // share count than calling buy_shares() directly - it wraps the exact
    // same production transition rather than reimplementing it, so this is
    // the guardrail against that ever drifting apart.
    #[test]
    fn quote_buy_matches_buy_shares_exactly() {
        let pools = [
            [7u64, 7u64],
            [7_071_067, 7_071_067],
            [100, 50],
            [1, 1_000_000],
        ]; // dial
        let deposits = [1u64, 1_000, 1_000_000, 50_000_000]; // dial

        for pool in pools {
            for &deposit in &deposits {
                let old_total = pool[0];
                let via_buy_shares = buy_shares(&pool, 0, deposit).map(|nt| nt - old_total);
                let via_quote = quote_buy(&pool, 0, deposit).map(|q| q.shares_received);

                println!(
                    "pool {pool:?}, deposit {deposit}: buy_shares={via_buy_shares:?}, quote_buy={via_quote:?}"
                );
                assert_eq!(
                    via_buy_shares.filter(|&d| d != 0),
                    via_quote,
                    "quote_buy disagreed with buy_shares for pool {pool:?}, deposit {deposit}"
                );
            }
        }
    }

    // Test F - min_shares_out comparison logic (docs/mvp.md 1.5's planned
    // replacement for the now-retired Config.slippage_limit_bps check): a
    // trade should succeed only when actual shares received meet or exceed
    // the caller's minimum. Tests the comparison in isolation — process_place_bet
    // doesn't take a min_shares_out argument yet, this is ahead of that wiring.
    #[test]
    fn min_shares_out_guard() {
        let pool = [7_071_067u64, 7_071_067];
        let deposit = 1_000_000u64;

        let quote = quote_buy(&pool, 0, deposit).expect("valid quote");
        let actual = quote.shares_received;

        // A minimum above what was actually received must be rejected.
        let too_high = actual + 1;
        assert!(
            actual < too_high,
            "test setup error: too_high must exceed actual"
        );
        println!("actual {actual}, min_shares_out {too_high} (too high) -> should fail");

        // A minimum at or below the actual amount must succeed.
        for min_shares_out in [actual, actual.saturating_sub(1), 0] {
            let would_succeed = actual >= min_shares_out;
            println!(
                "actual {actual}, min_shares_out {min_shares_out} -> succeeds: {would_succeed}"
            );
            assert!(would_succeed);
        }
        let would_succeed_too_high = actual >= too_high;
        assert!(
            !would_succeed_too_high,
            "a min_shares_out above actual must fail"
        );
    }

    // A human-readable worked example, generated entirely from real
    // quote_buy() output - not hard-coded illustrative numbers. Run with
    // `cargo test -p metis-core --lib amm::curve -- --nocapture` to see
    // it. This is the same shape the research crate's reporting layer
    // reuses for its own single-transaction-quote experiment.
    #[test]
    fn single_transaction_quote_human_readable_example() {
        let pool = [7_071_067u64, 7_071_067]; // dial: a realistic ~$10-scale market
        let deposit = 1_000_000u64; // dial: ~$1 deposit at this scale
        let side = 0usize;

        let quote = quote_buy(&pool, side, deposit).expect("valid quote");
        let mut after = pool;
        after[side] += quote.shares_received;

        println!();
        println!("{}", "=".repeat(60));
        println!("METIS — SINGLE TRANSACTION QUOTE");
        println!("{}", "=".repeat(60));
        println!();
        println!("Initial market");
        println!();
        println!("YES shares:        {}", pool[0]);
        println!("NO shares:         {}", pool[1]);
        println!("Curve cost:        {}", quote.current_curve_cost);
        println!(
            "YES probability:   {}.{:02}%",
            quote.probability_before_bps / 100,
            quote.probability_before_bps % 100
        );
        println!();
        println!("User trade");
        println!();
        println!("Deposit:           {deposit}");
        println!("Outcome:           YES");
        println!();
        println!("Curve transition");
        println!();
        println!("Shares received:   {}", quote.shares_received);
        println!("New YES shares:    {}", after[0]);
        println!("New curve cost:    {}", quote.new_curve_cost);
        println!();
        println!("Execution");
        println!();
        // Basis points -> percentage-with-two-decimals is plain integer
        // div/mod, no float anywhere in this formatting.
        let pct = |bps: u32| (bps / 100, bps % 100);
        let (before_whole, before_frac) = pct(quote.probability_before_bps);
        let (after_whole, after_frac) = pct(quote.probability_after_bps);
        let (impact_whole, impact_frac) = pct(quote.price_impact_bps);

        // The execution price's scaled integer, split into whole and
        // fractional quote-units-per-share for display, again with no
        // float involved.
        let exec_whole = quote.effective_execution_price_scaled / PRICE_SCALE;
        let exec_frac = quote.effective_execution_price_scaled % PRICE_SCALE;

        println!("Average execution: {exec_whole}.{exec_frac:09} / YES");
        println!();
        println!("Probability");
        println!();
        println!("Before:             {before_whole}.{before_frac:02}%");
        println!("After:              {after_whole}.{after_frac:02}%");
        println!();
        println!("Price impact:       {impact_whole}.{impact_frac:02}pp");
        println!("{}", "=".repeat(60));
        println!();
    }

    // The same properties again, but against randomly generated pools rather
    // than hand-picked ones. Proptest prints nothing when it passes - a failure
    // prints the exact inputs that broke it, which is the point.
    //
    // Bounds stay well under u64::MAX so a squared sum can't approach
    // u128::MAX. This is about realistic market states, not fuzzing raw integer
    // overflow, which the checked_* call sites guard separately.
    proptest! {
        #[test]
        fn buy_shares_is_monotonic_in_payment_random(
            a in 1u64..1_000_000_000_000,
            b in 1u64..1_000_000_000_000,
            small_payment in 1u64..1_000_000_000,
            extra_payment in 0u64..1_000_000_000,
        ) {
            let pool = [a, b];
            let bigger_payment = small_payment.saturating_add(extra_payment);

            let small_new = buy_shares(&pool, 0, small_payment).expect("should not overflow at this scale");
            let bigger_new = buy_shares(&pool, 0, bigger_payment).expect("should not overflow at this scale");

            prop_assert!(
                bigger_new >= small_new,
                "a larger-or-equal payment must never buy fewer shares (pool={pool:?}, small={small_payment}, bigger={bigger_payment})"
            );
        }

        #[test]
        fn buy_shares_never_shrinks_the_bought_side(
            a in 1u64..1_000_000_000_000,
            b in 1u64..1_000_000_000_000,
            payment in 1u64..1_000_000_000,
            side in 0usize..2,
        ) {
            let pool = [a, b];
            let new_total = buy_shares(&pool, side, payment).expect("should not overflow at this scale");
            prop_assert!(new_total >= pool[side], "buying can never reduce a side's own share total");
        }

        #[test]
        fn price_is_always_a_valid_probability_component(
            a in 1u64..1_000_000_000_000,
            b in 1u64..1_000_000_000_000,
        ) {
            let pool = [a, b];
            let price_a = price(&pool, 0);
            let price_b = price(&pool, 1);
            prop_assert!((0.0..=1.0).contains(&price_a), "price must be a valid [0,1] fraction, got {price_a}");
            prop_assert!((0.0..=1.0).contains(&price_b), "price must be a valid [0,1] fraction, got {price_b}");
        }

        // cost() must never round up. If it did, the pool would measure itself
        // as holding more than was ever paid into it.
        #[test]
        fn cost_never_rounds_up(
            a in 0u64..1_000_000_000_000,
            b in 0u64..1_000_000_000_000,
        ) {
            let pool = [a, b];
            let c = cost(&pool);
            let sum_sq = (a as u128) * (a as u128) + (b as u128) * (b as u128);
            prop_assert!(c * c <= sum_sq, "cost rounded up for pool {pool:?}");
        }

        // The seed must never mint more than the fee bought, at any fee or
        // outcome count. This is the one that would catch a bad guard.
        #[test]
        fn symmetric_seed_never_mints_more_than_the_fee(
            fee in 0u64..1_000_000_000_000,
            outcomes in 2u8..=u8::MAX,
        ) {
            let seed = symmetric_seed(fee, outcomes);
            let total = isqrt((seed as u128) * (seed as u128) * outcomes as u128);
            prop_assert!(
                total <= fee as u128,
                "seeding {outcomes} outcomes from a fee of {fee} produced a pool measuring {total}"
            );
        }

        // CORRECTION (twice over — recorded here so the mistake isn't
        // repeated): earlier versions of this test claimed priced_shares()
        // makes buy_shares()'s delta invariant to the virtual offset, first
        // for any pool, then (after that failed) for symmetric pools only.
        // Both are false as a general law, not just floor-rounding noise:
        //
        // For a symmetric pool [x, x], buying side 0 with payment P gives
        // Delta(x) = sqrt(x^2 + 2*sqrt(2)*P*x + P^2) - x, and d/dx Delta(x)
        // is STRICTLY POSITIVE (verified algebraically) - so more depth
        // (virtual liquidity) makes a symmetric-pool trade buy MORE shares
        // for the same payment, converging toward the frictionless ceiling
        // Delta(x) -> sqrt(2)*P as x -> infinity (less slippage = closer to
        // the naive/no-friction count, which is a LARGER share count, per
        // this file's own buying_always_gets_less_than_a_flat_price_would_suggest).
        //
        // That's the OPPOSITE direction from the lopsided-pool counterexample
        // below (real=25 -> priced=16, a decrease). The sign is geometry- and
        // side-dependent; there is no universal "virtual liquidity always
        // raises/lowers the delta" property to assert here. What IS always
        // true, and is what docs/mvp.md's protections actually care about, is
        // price impact (bps) shrinking - see
        // virtual_liquidity_shrinks_price_impact_vs_mvp_worked_example below,
        // a single pinned worked example rather than a false generalization.
        //
        // place_bet.rs was never affected by any of this: it always computes
        // delta straight from quote_buy(&priced, ...), never assumed an
        // invariance shortcut.
    }

    // ── virtual liquidity (docs/mvp.md 1.4) ────────────────────────────

    // Pins the exact counterexample that disproved the earlier "delta is
    // always offset-invariant" claim above: on a lopsided pool, buying into
    // the MINORITY side with virtual liquidity applied buys FEWER shares for
    // the same payment than the real book alone would give. That's the
    // dampening effect working as intended - the priced curve is flatter, so
    // a payment can't extract as sharp a swing on a thin, unfavoured side.
    #[test]
    fn virtual_liquidity_dampens_delta_on_a_lopsided_pool() {
        let pool = [29_568_370u64, 1_000_000u64]; // dial: a real, lopsided book
        let v = 9u64; // dial: a small virtual offset
        let payment = 1u64; // dial
        let side = 1usize; // the minority/underdog side

        let real_new = buy_shares(&pool, side, payment).expect("no overflow at this scale");
        let real_delta = real_new - pool[side];

        let priced = priced_shares(&pool, v).expect("no overflow at this scale");
        let priced_new = buy_shares(&priced, side, payment).expect("no overflow at this scale");
        let priced_delta = priced_new - priced[side];

        println!(
            "lopsided pool {pool:?}, payment {payment} into the minority side: \
             real_delta={real_delta}, priced_delta={priced_delta} (virtual v={v})"
        );

        assert_ne!(
            real_delta, priced_delta,
            "virtual liquidity should change the delta on a lopsided pool, not leave it untouched"
        );
        assert!(
            priced_delta < real_delta,
            "virtual liquidity should dampen the swing on the underdog side, not amplify it \
             (real={real_delta}, priced={priced_delta})"
        );
    }

    // virtual_liquidity_per_side is just symmetric_seed(fee, 2) x k — this
    // checks that reuse against the docs/mvp.md 1.4 worked example
    // (seed = $20, k = 30 -> virtual_per_side ~= 424.26).
    #[test]
    fn virtual_liquidity_matches_mvp_worked_example() {
        let seed = crate::constants::MIN_SEED; // dial: $20, MVP's fixed seed
        let k = 30u64; // dial: docs/mvp.md 1.4's k

        let q0 = symmetric_seed(seed, 2);
        let virtual_per_side =
            virtual_liquidity_per_side(q0, k).expect("no overflow at this scale");
        let dollars = virtual_per_side as f64 / 1_000_000.0;

        println!(
            "seed {seed} (2 outcomes) -> symmetric_seed = {q0}, k = {k} \
             -> virtual_per_side = {virtual_per_side} (~{dollars:.2} at 6-decimal USDC scale)"
        );

        assert_eq!(virtual_per_side, q0 * k, "must just be q0 scaled by k");
        assert!(
            (dollars - 424.26).abs() < 1.0,
            "expected roughly docs/mvp.md 1.4's worked example (~424.26), got {dollars:.2}"
        );
    }

    // priced_shares must fail closed (None), never wrap, when the virtual
    // offset would push a side past u64::MAX - same convention as every
    // other checked_* call in this module.
    #[test]
    fn priced_shares_overflow_returns_none() {
        let pool = [u64::MAX, 0u64];
        assert!(
            priced_shares(&pool, 1).is_none(),
            "adding virtual liquidity past u64::MAX must fail closed, not wrap"
        );

        let healthy = [1_000u64, 2_000];
        let got = priced_shares(&healthy, 500).expect("no overflow at this scale");
        assert_eq!(got, [1_500, 2_500]);
    }

    // The demonstration this whole mechanism exists for: docs/mvp.md 1.3
    // cites a $50 first trade into a $20 seed measuring 7.18pp of price
    // impact with NO virtual liquidity. The same trade against the same
    // seed, once k=30 virtual liquidity is applied, must move the market
    // materially less - that shrinkage is the entire point of 1.4.
    #[test]
    fn virtual_liquidity_shrinks_price_impact_vs_mvp_worked_example() {
        let seed = crate::constants::MIN_SEED; // dial: $20
        let bet = 50_000_000u64; // dial: $50
        let k = 30u64; // dial

        let q0 = symmetric_seed(seed, 2);
        let pool = [q0, q0];

        let without = quote_buy(&pool, 0, bet).expect("valid quote");

        let v = virtual_liquidity_per_side(q0, k).expect("no overflow at this scale");
        let priced_pool = priced_shares(&pool, v).expect("no overflow at this scale");
        let with = quote_buy(&priced_pool, 0, bet).expect("valid quote");

        let pp = |bps: u32| bps as f64 / 100.0;
        println!(
            "without virtual liquidity: {:.2}pp impact (docs/mvp.md 1.3 cites ~7.18pp)",
            pp(without.price_impact_bps)
        );
        println!(
            "with virtual liquidity (k={k}): {:.2}pp impact",
            pp(with.price_impact_bps)
        );

        assert!(
            with.price_impact_bps < without.price_impact_bps,
            "virtual liquidity should shrink price impact for the same trade, \
             got without={} bps, with={} bps",
            without.price_impact_bps,
            with.price_impact_bps
        );
    }
}
