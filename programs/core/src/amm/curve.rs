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
    let mut y = (x + 1) / 2;

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



#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn isqrt_perfect_squares() {
        assert_eq!(isqrt(0), 0);
        assert_eq!(isqrt(1), 1);
        assert_eq!(isqrt(100), 10);
        assert_eq!(isqrt(10_000), 100);
    }

    #[test]
    fn isqrt_floors_non_squares() {
        assert_eq!(isqrt(98), 9); // 9x9=81, 10x10=100, so it rounds down to 9
        assert_eq!(isqrt(99), 9);
        assert_eq!(isqrt(101), 10);
    }

    #[test]
    fn symmetric_seed_matches_design_doc_example() {
        // $10 fee, 2 outcomes -> 10/sqrt(2) is about 7.07, rounds down to 7.
        assert_eq!(symmetric_seed(10, 2), 7);
    }

    #[test]
    fn cost_of_symmetric_seed_is_close_to_fee() {
        let q0 = symmetric_seed(10, 2);
        // Rounds down twice (seeding, then cost), so it lands on 9, not 10.
        assert_eq!(cost(&[q0, q0]), 9);
    }

    #[test]
    fn buy_shares_matches_design_doc_slippage_example() {
        // A $13 bet into a $10 pool should get noticeably fewer shares
        // than a naive guess of ~19 — that gap is expected, not a bug.
        let q0 = symmetric_seed(10, 2);
        let pool = [q0, q0];

        let new_total = buy_shares(&pool, 0, 13).expect("no underflow");
        let shares_received = new_total - q0;

        assert!(
            shares_received < 19,
            "got {shares_received}, expected well under the naive ~19 estimate"
        );
    }

    #[test]
    fn buy_shares_never_underflows_on_a_healthy_market() {
        let pool = [7u64, 7u64];
        for payment in [1u64, 5, 50, 1_000] {
            assert!(buy_shares(&pool, 0, payment).is_some());
            assert!(buy_shares(&pool, 1, payment).is_some());
        }
    }

    // --- Property checks (production-tooling pass) ---

    #[test]
    fn buy_shares_never_underflows_and_is_monotonic_in_payment() {
        // A real deposit should never panic/underflow, and a bigger payment
        // should never buy fewer shares than a smaller one on the same pool —
        // across a spread of pool shapes, not just the one worked example above.
        // (Floor rounding means a tiny payment can legitimately mint a delta of
        // 0 against a huge pool — that's not a bug, so this checks monotonicity
        // instead of "always > 0".)
        let pools = [[7u64, 7u64], [100, 50], [1, 1_000_000], [500_000, 500_000]];
        let payments = [1u64, 10, 12_345, 1_000_000];

        for pool in pools {
            for side in 0..2usize {
                let mut last_delta = 0u64;
                for &payment in &payments {
                    let new_total = buy_shares(&pool, side, payment)
                        .expect("healthy pool + payment should never overflow/underflow");
                    let delta = new_total - pool[side];
                    assert!(
                        delta >= last_delta,
                        "a larger payment must never buy fewer shares (pool={pool:?}, side={side}, payment={payment}, delta={delta}, previous={last_delta})"
                    );
                    last_delta = delta;
                }
            }
        }
    }

    #[test]
    fn price_squared_sums_to_probability_one_and_drift_shrinks_at_scale() {
        // The real invariant here is on PROBABILITY, not price: price_i =
        // q_i / sqrt(sum q^2), so price_i^2 = q_i^2 / sum(q^2), and summing
        // price_i^2 across every outcome is exactly 1 by construction — that's
        // what the scenario tests' printed "(49.6%)"-style figures are (price
        // squared, not price itself; two prices on a 2-outcome market do NOT
        // sum to 1 — e.g. 0.7071 + 0.7071 = 1.414, not 1).
        //
        // The known, documented rounding drift (see arch/USER_FLOWS.md) is that
        // an integer isqrt in the denominator means this sum isn't always
        // exactly 1.0 at a tiny pool size. This test quantifies that drift and
        // asserts it shrinks at the real MARKET_INIT_FEE-sized regime.
        let tiny_pool = [7u64, 7u64];
        let real_scale_pool = [7_071_067u64, 7_071_067u64];

        let tiny_prob_sum = price(&tiny_pool, 0).powi(2) + price(&tiny_pool, 1).powi(2);
        let real_scale_prob_sum = price(&real_scale_pool, 0).powi(2) + price(&real_scale_pool, 1).powi(2);

        let tiny_drift = (tiny_prob_sum - 1.0).abs();
        let real_scale_drift = (real_scale_prob_sum - 1.0).abs();

        assert!(
            real_scale_drift <= tiny_drift,
            "expected drift to shrink (or stay flat) at real scale (tiny={tiny_drift:.6}, real={real_scale_drift:.6})"
        );
        // At real scale, the drift should be negligible — a few parts per million.
        assert!(real_scale_drift < 0.001, "real-scale drift {real_scale_drift:.6} is larger than expected");
    }

    // --- proptest: same properties, randomized instead of hand-picked pools ---
    // Bounds kept well under u64::MAX so a squared sum can never approach
    // u128::MAX (u64::MAX^2 * 2 is already close to overflowing u128) — this is
    // about exercising realistic-scale market states, not fuzzing raw integer
    // overflow, which the checked_add call sites already guard separately.
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
    }
}
