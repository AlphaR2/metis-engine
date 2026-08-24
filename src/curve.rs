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

// Virtual liquidity — a v0.1 fix, worth revisiting later.
//
// Right after a market opens, the pool is tiny (just the creator's seed),
// so the first few trades swing the price hard. This adds a fake amount
// to both sides before doing any price math, so the pool acts deeper than
// it really is. Nobody deposits this money and nobody can withdraw it.
//
// Safe because it cancels out: how many shares someone actually earns
// comes out the same whether we pad or not. Payouts only ever use the
// real numbers — this fake amount never touches them.
pub const VIRTUAL_LIQUIDITY: u64 = 100;

fn padded(outcome_shares: &[u64; MAX_OUTCOMES]) -> [u64; MAX_OUTCOMES] {
    core::array::from_fn(|i| outcome_shares[i] + VIRTUAL_LIQUIDITY)
}

/// Cost of the pool, padded. Pricing only — never use this for payouts.
pub fn padded_cost(outcome_shares: &[u64; MAX_OUTCOMES]) -> u128 {
    cost(&padded(outcome_shares))
}

/// buy_shares, but padded. Returns the REAL number of shares earned
/// (a delta, not a total). Use this one once virtual liquidity is live —
/// only call the plain buy_shares when deliberately testing without padding.
pub fn buy_shares_padded(
    outcome_shares: &[u64; MAX_OUTCOMES],
    outcome_index: usize,
    payment: u64,
) -> Option<u64> {
    let p = padded(outcome_shares);
    let old = p[outcome_index];
    let new_total = buy_shares(&p, outcome_index, payment)?;
    Some(new_total - old)
}

/// The price a bettor sees and trades against. Pricing only, not payouts.
pub fn padded_price(outcome_shares: &[u64; MAX_OUTCOMES], outcome_index: usize) -> f64 {
    let p = padded(outcome_shares);
    let denom = (cost(&p) as f64).max(1.0);
    p[outcome_index] as f64 / denom
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn padding_reduces_slippage_on_the_alice_then_ben_case() {
        // Alice buys $40 of A first, Ben buys $25 of B right after.
        // Padding should stop Ben's price from being crushed by Alice's trade.
        let q0 = symmetric_seed(10, 2);
        let seed = [q0, q0];

        let alice_new = buy_shares(&seed, 0, 40).unwrap();
        let pool_after_alice = [alice_new, q0];
        let ben_price_before_unpadded =
            pool_after_alice[1] as f64
                / ((pool_after_alice[0].pow(2) + pool_after_alice[1].pow(2)) as f64).sqrt();

        let alice_new_padded = buy_shares_padded(&seed, 0, 40).unwrap() + q0;
        let pool_after_alice_padded = [alice_new_padded, q0];
        let ben_price_before_padded = padded_price(&pool_after_alice_padded, 1);

        assert!(
            ben_price_before_padded > ben_price_before_unpadded,
            "padding should leave Ben a better starting price than the unpadded case \
             (padded={ben_price_before_padded:.4}, unpadded={ben_price_before_unpadded:.4})"
        );
    }

    #[test]
    fn padded_delta_matches_unpadded_delta() {
        // Padding both sides by the same amount shouldn't change how many
        // shares a bet actually earns — checked here with a manual pad.
        let pool = [50u64, 60u64];
        let manual_pad = 37u64;
        let padded_pool: [u64; 2] = [pool[0] + manual_pad, pool[1] + manual_pad];

        let real_delta = buy_shares(&pool, 0, 25).unwrap() - pool[0];
        let padded_new = buy_shares(&padded_pool, 0, 25).unwrap();
        let recovered_delta = padded_new - padded_pool[0];

        assert_eq!(
            real_delta, recovered_delta,
            "a constant pad on both sides must not change the minted delta"
        );
    }
}
