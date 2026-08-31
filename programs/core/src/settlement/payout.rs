
/// The vault's expected lamport balance right before settlement: rent-exempt
/// minimum plus every deposit ever collected. Only ever exactly true at that
/// one moment 
pub fn expected_vault_balance(rent_minimum: u64, total_collected: u64) -> Option<u64> {
    rent_minimum.checked_add(total_collected)
}

/// payout = my winning shares * total collected / all winning shares.
/// Zero if this position never held the winning side. Floor division,
pub fn compute_payout(winning_shares: u64, total_collected: u64, all_winning_shares: u64) -> u64 {
    if all_winning_shares == 0 || winning_shares == 0 {
        return 0;
    }
    ((winning_shares as u128) * (total_collected as u128) / (all_winning_shares as u128)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn expected_vault_balance_adds_rent_and_collected() {
        assert_eq!(expected_vault_balance(1_000, 500_000), Some(501_000));
    }

    #[test]
    fn expected_vault_balance_none_on_overflow() {
        assert_eq!(expected_vault_balance(u64::MAX, 1), None);
    }

    #[test]
    fn compute_payout_splits_proportionally() {
        // 3 winning shares out of 10 total, $100 collected -> $30.
        assert_eq!(compute_payout(3, 100, 10), 30);
    }

    #[test]
    fn compute_payout_zero_when_no_winning_shares() {
        assert_eq!(compute_payout(0, 1_000, 500), 0);
    }

    #[test]
    fn compute_payout_zero_when_pool_had_no_winners() {
        // Shouldn't happen in practice (creator always seeds both sides), but
        // must not panic/divide-by-zero if it ever does.
        assert_eq!(compute_payout(5, 1_000, 0), 0);
    }

    #[test]
    fn compute_payout_floors_and_never_overflows_at_scale() {
        // Realistic large-market numbers: doesn't overflow, and rounds down
        // rather than panicking or rounding up past what the pool holds.
        let winning_shares = 253_488_139u64;
        let all_winning_shares = 255_624_212u64;
        let total_collected = 360_000_000u64; // $360.00 in 1e6 base units

        let payout = compute_payout(winning_shares, total_collected, all_winning_shares);
        assert!(payout <= total_collected, "a single payout can never exceed the whole pool");
        assert!(payout > 0);
    }

    #[test]
    fn compute_payout_sums_to_at_most_total_collected_across_many_winners() {
        // Floor division means the sum of every winner's payout is <=
        // total_collected, never more (the "dust stays in the pool" invariant).
        let all_winning_shares = 1_000u64;
        let total_collected = 999_999u64; // deliberately not evenly divisible
        let per_winner_shares = 37u64; // doesn't divide 1000 evenly either

        let mut distributed = 0u128;
        let mut remaining_shares = all_winning_shares;
        while remaining_shares > 0 {
            let shares = per_winner_shares.min(remaining_shares);
            distributed += compute_payout(shares, total_collected, all_winning_shares) as u128;
            remaining_shares -= shares;
        }

        assert!(distributed <= total_collected as u128);
    }

    // --- proptest: the one invariant that actually matters, randomized ---
    proptest! {
        #[test]
        fn compute_payout_never_exceeds_total_collected_random(
            winning_shares in 0u64..1_000_000_000_000,
            all_winning_shares in 0u64..1_000_000_000_000,
            total_collected in 0u64..1_000_000_000_000,
        ) {
            // winning_shares can't validly exceed all_winning_shares in real
            // usage (a position's own shares are always <= the pool total) —
            // constrain the generated inputs to that real invariant instead of
            // asserting on nonsensical combinations.
            prop_assume!(winning_shares <= all_winning_shares);

            let payout = compute_payout(winning_shares, total_collected, all_winning_shares);
            prop_assert!(payout <= total_collected, "a single payout can never exceed the whole pool");
        }
    }
}
