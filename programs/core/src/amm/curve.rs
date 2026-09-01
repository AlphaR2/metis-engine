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

// Tests for the pricing curve. To see the printed output:
//   cargo test -p openstake-core --lib amm::curve -- --nocapture --test-threads=1
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
    // Every real market seeds with MARKET_INIT_FEE = 10_000_000 (millionths),
    // not a toy number like 100. At tiny integer scale isqrt's floor-rounding
    // isn't a small correction any more, it's the dominant effect, and the
    // "bigger bet slips more" property gets swamped by rounding noise - try
    // seed_fee = 100 with these same bets and watch slippage jump around
    // instead of climbing smoothly. That's not a bug in the curve; it's what
    // "the maths only holds at scale" actually looks like when shrunk too far.
    #[test]
    fn slippage_tracks_bet_size_against_pool_depth() {
        let seed_fee = 100_000_000u64; // dial: pool depth, MARKET_INIT_FEE scale
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
    }
}
