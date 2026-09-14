// expected_vault_balance used to live here, adding the rent-exempt minimum to
// total_collected. It's gone: the vault is now an SPL token account, so its
// rent is paid in SOL and sits outside the token balance. The solvency check
// in settle_market is just "amount >= total_collected", no rent term needed.

/// payout = my winning shares * total collected / all winning shares.
///
/// Every winning share is worth the same slice of the pool, no matter what
/// anyone paid to get it. Floor division, so the pool can only ever be
/// short-changed in its own favour - a few units of dust stay behind rather
/// than the vault promising more than it holds.
pub fn compute_payout(winning_shares: u64, total_collected: u64, all_winning_shares: u64) -> u64 {
    if all_winning_shares == 0 || winning_shares == 0 {
        return 0;
    }
    ((winning_shares as u128) * (total_collected as u128) / (all_winning_shares as u128)) as u64
}

// Tests for the pari-mutuel payout. To see the printed output:
//   cargo test -p metis-core --lib settlement::payout -- --nocapture
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // The basic promise: hold 3 of the 10 winning shares, get 3/10 of the pool.
    // Nothing about what anyone paid enters into it.
    #[test]
    fn compute_payout_splits_proportionally() {
        let payout = compute_payout(3, 100, 10);
        println!("3 of 10 winning shares, pool 100 -> payout {payout}");
        assert_eq!(payout, 30);
    }

    // Holding none of the winning side pays nothing. This is the losing
    // bettor's path, and it must be exactly zero, not a rounding artefact.
    #[test]
    fn compute_payout_zero_when_no_winning_shares() {
        let payout = compute_payout(0, 1_000, 500);
        println!("0 winning shares, pool 1000 -> payout {payout}");
        assert_eq!(payout, 0);
    }

    // Shouldn't be reachable in practice, since the creator always seeds every
    // side. But an empty winning side would be a divide-by-zero, so it has to
    // return 0 rather than panic and take the whole instruction down.
    #[test]
    fn compute_payout_zero_when_pool_had_no_winners() {
        let payout = compute_payout(5, 1_000, 0);
        println!("5 shares but the winning side is empty -> payout {payout} (no divide-by-zero)");
        assert_eq!(payout, 0);
    }

    // Real market-sized numbers: no overflow, and it rounds down rather than
    // promising more than the pool holds.
    #[test]
    fn compute_payout_floors_and_never_overflows_at_scale() {
        let winning_shares = 253_488_139u64;
        let all_winning_shares = 255_624_212u64;
        let total_collected = 360_000_000u64; // $360.00 in millionths

        let payout = compute_payout(winning_shares, total_collected, all_winning_shares);
        println!(
            "holding {:.2} of {:.2} winning shares, pool ${:.2} -> payout ${:.2}",
            winning_shares as f64 / 1e6,
            all_winning_shares as f64 / 1e6,
            total_collected as f64 / 1e6,
            payout as f64 / 1e6
        );

        assert!(
            payout <= total_collected,
            "a single payout can never exceed the whole pool"
        );
        assert!(payout > 0);
    }

    // The invariant the whole design rests on: add up what every winner takes
    // and it can never come to more than what went in. Rounding down means the
    // total lands slightly UNDER, leaving dust behind - which is the safe
    // direction to be wrong in.
    #[test]
    fn compute_payout_sums_to_at_most_total_collected_across_many_winners() {
        let all_winning_shares = 1_000u64;
        let total_collected = 999_999u64; // deliberately not evenly divisible
        let per_winner_shares = 37u64; // doesn't divide 1000 evenly either

        let mut distributed = 0u128;
        let mut remaining_shares = all_winning_shares;
        let mut claims = 0;
        while remaining_shares > 0 {
            let shares = per_winner_shares.min(remaining_shares);
            distributed += compute_payout(shares, total_collected, all_winning_shares) as u128;
            remaining_shares -= shares;
            claims += 1;
        }

        println!("{claims} winners splitting a pool of {total_collected}");
        println!("  paid out: {distributed}");
        println!(
            "  dust left behind: {}",
            total_collected as u128 - distributed
        );

        assert!(distributed <= total_collected as u128);
    }

    // Same invariant, randomised. Proptest prints nothing when it passes - if
    // it fails it prints the exact inputs that broke it, which is the point.
    proptest! {
        #[test]
        fn compute_payout_never_exceeds_total_collected_random(
            winning_shares in 0u64..1_000_000_000_000,
            all_winning_shares in 0u64..1_000_000_000_000,
            total_collected in 0u64..1_000_000_000_000,
        ) {
            // A position can't hold more winning shares than exist, so constrain
            // the generated inputs to that real-world invariant rather than
            // asserting things about impossible states.
            prop_assume!(winning_shares <= all_winning_shares);

            let payout = compute_payout(winning_shares, total_collected, all_winning_shares);
            prop_assert!(payout <= total_collected, "a single payout can never exceed the whole pool");
        }
    }
}
