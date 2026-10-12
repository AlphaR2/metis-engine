// expected_vault_balance used to live here, adding the rent-exempt minimum to
// total_collected. It's gone: the vault is now an SPL token account, so its
// rent is paid in SOL and sits outside the token balance. The solvency check
// in settle_market is just "amount >= total_collected", no rent term needed.

/// payout = my winning shares * payout pool / all winning shares (the pool is the pot minus the
/// rake once settled, or the whole vault once cancelled).
///
/// Every winning share is worth the same slice of the pool, no matter what
/// anyone paid to get it. Floor division, so the pool can only ever be
/// short-changed in its own favour - a few units of dust stay behind rather
/// than the vault promising more than it holds.
pub fn compute_payout(winning_shares: u64, pool: u64, all_winning_shares: u64) -> u64 {
    if all_winning_shares == 0 || winning_shares == 0 {
        return 0;
    }
    ((winning_shares as u128) * (pool as u128) / (all_winning_shares as u128)) as u64
}

/// How a market's revenue (the rake plus every bet fee) is shared once it settles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RevenueSplit {
    pub protocol: u64,
    pub backers: u64,
    pub creator: u64,
    pub platform: u64,
}

impl RevenueSplit {
    pub fn total(&self) -> u64 {
        self.protocol + self.backers + self.creator + self.platform
    }
}

/// Splits `revenue` four ways by the market's snapshotted shares, given in basis points as
/// `[protocol, backers, creator, platform]`.
///
/// Each of the other three is rounded down and the protocol takes the remainder, so the four
/// always add up to exactly `revenue` and the rounding dust lands with the protocol, never in the
/// vault. Two folds keep every atom paid to someone real:
///   - a creator-seeded market has no backers, so the backers' share goes to the creator (who
///     holds the seed);
///   - a market with no platform leaves that share with the protocol.
pub fn split_revenue(revenue: u64, shares_bps: [u16; 4], seed_is_creator: bool, has_platform: bool) -> RevenueSplit {
    let cut = |bps: u16| ((revenue as u128 * bps as u128) / crate::constants::BPS_DENOMINATOR as u128) as u64;
    let mut backers = cut(shares_bps[1]);
    let mut creator = cut(shares_bps[2]);
    let platform = if has_platform { cut(shares_bps[3]) } else { 0 };
    if seed_is_creator {
        creator += backers;
        backers = 0;
    }
    // The shares sum to at most 100%, so these cuts can never exceed the revenue.
    let protocol = revenue - backers - creator - platform;
    RevenueSplit { protocol, backers, creator, platform }
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

    // The four legs, on numbers small enough to check by hand: 1,000 of revenue at
    // 40 / 30 / 20 / 10 percent.
    #[test]
    fn revenue_splits_four_ways_and_adds_up() {
        let s = split_revenue(1_000, [4_000, 3_000, 2_000, 1_000], false, true);
        assert_eq!(s, RevenueSplit { protocol: 400, backers: 300, creator: 200, platform: 100 });
        assert_eq!(s.total(), 1_000);
    }

    // A creator-seeded market has no backers: their share is the creator's, not the protocol's.
    #[test]
    fn a_creator_seeded_market_folds_the_backers_share_into_the_creator() {
        let s = split_revenue(1_000, [4_000, 3_000, 2_000, 1_000], true, true);
        assert_eq!(s, RevenueSplit { protocol: 400, backers: 0, creator: 500, platform: 100 });
    }

    // No platform named at creation: that share stays with the protocol rather than vanishing.
    #[test]
    fn a_market_with_no_platform_leaves_its_share_with_the_protocol() {
        let s = split_revenue(1_000, [4_000, 3_000, 2_000, 1_000], false, false);
        assert_eq!(s, RevenueSplit { protocol: 500, backers: 300, creator: 200, platform: 0 });
    }

    // Rounding: the three smaller legs round down and the protocol takes whatever is left, so the
    // legs add up to the revenue exactly, even when nothing divides evenly.
    #[test]
    fn rounding_dust_goes_to_the_protocol_and_nothing_is_lost() {
        let s = split_revenue(7, [4_000, 3_000, 2_000, 1_000], false, true);
        assert_eq!(s.total(), 7);
        assert_eq!((s.backers, s.creator, s.platform), (2, 1, 0));
        assert_eq!(s.protocol, 4);
        assert_eq!(split_revenue(0, [4_000, 3_000, 2_000, 1_000], false, true).total(), 0);
    }

    proptest! {
        // For any revenue and any valid set of shares, in any seeding mode, the four legs add up
        // to the revenue exactly, and the protocol never gets less than its stated share.
        #[test]
        fn the_four_legs_always_add_up_to_the_revenue(
            revenue in 0u64..u64::MAX / 2,
            protocol in 0u16..=crate::constants::MAX_PROTOCOL_SHARE_BPS,
            backers_fraction in 0u16..=100,
            creator_fraction in 0u16..=100,
            seed_is_creator in proptest::bool::ANY,
            has_platform in proptest::bool::ANY,
        ) {
            let rest = 10_000 - protocol;
            let backers = (rest as u32 * backers_fraction as u32 / 100) as u16;
            let creator = ((rest - backers) as u32 * creator_fraction as u32 / 100) as u16;
            let platform = rest - backers - creator;
            let s = split_revenue(revenue, [protocol, backers, creator, platform], seed_is_creator, has_platform);
            prop_assert_eq!(s.total(), revenue);
            let floor = ((revenue as u128 * protocol as u128) / 10_000) as u64;
            prop_assert!(s.protocol >= floor, "protocol {} below its share {}", s.protocol, floor);
        }
    }

    // The whole settlement in one property: for ANY sequence of bets, ANY fee, rake and fee
    // split within their hardcoded ceilings, and EITHER side winning, what goes out never
    // exceeds what came in, and only rounding dust (at most an atom or two per holder) is left.
    //
    // This is the sum the vault's balance has to explain at the end, and it uses the same
    // functions the program runs: the fee, the curve, the rake, the fee split and the payout.
    proptest! {
        #[test]
        fn payouts_plus_rake_plus_fees_equal_what_went_in(
            bets in proptest::collection::vec((0usize..2, 1_000_000u64..2_000_000_000), 1..25),
            fee_bps in 0u16..=crate::constants::MAX_FEE_PRIMARY_BPS,
            rake_bps in 0u16..=crate::constants::MAX_RAKE_BPS,
            protocol_share in 0u16..=crate::constants::MAX_PROTOCOL_SHARE_BPS,
            platform_share in 0u16..=2_000,
            seed_is_creator in proptest::bool::ANY,
            has_platform in proptest::bool::ANY,
            winner in 0usize..2,
            k in 0u64..=crate::constants::MAX_VIRTUAL_LIQUIDITY_MULTIPLE,
        ) {
            use crate::amm::{curve::{quote_buy, priced_shares, symmetric_seed, virtual_liquidity_per_side},
                             quote::{fee_of, rake_of}};
            use crate::constants::MIN_SEED;

            let seed_each = symmetric_seed(MIN_SEED, 2);
            let v = virtual_liquidity_per_side(seed_each, k).unwrap();
            let mut shares = [seed_each, seed_each];
            let mut pot = MIN_SEED;
            let mut fees = 0u64;
            let mut gross_in = MIN_SEED;
            // (shares held, side) for every holder: the creator's seed holds both sides.
            let mut holders: Vec<(u64, usize)> = vec![(seed_each, 0), (seed_each, 1)];

            for (side, deposit) in bets {
                let fee = fee_of(deposit, fee_bps);
                let net = deposit - fee;
                let priced = priced_shares(&shares, v).unwrap();
                // Bets the program would refuse (dust, impact ceiling) are simply not placed.
                let Some(q) = quote_buy(&priced, side, net) else { continue };
                if q.price_impact_bps > crate::constants::MAX_PRICE_IMPACT_BPS as u32 { continue; }
                shares[side] += q.shares_received;
                pot += net;
                fees += fee;
                gross_in += deposit;
                holders.push((q.shares_received, side));
            }

            // Settlement, exactly as the program does it.
            let rake = rake_of(pot, rake_bps);
            let payout_pool = pot - rake;
            let split = split_revenue(rake + fees, [protocol_share, 3_000, 10_000 - protocol_share - 3_000 - platform_share, platform_share], seed_is_creator, has_platform);
            prop_assert_eq!(split.total(), rake + fees, "the four legs must add up to the revenue exactly");
            let protocol_owed = split.protocol;
            let creator_owed = split.creator + split.backers + split.platform;

            let winners: Vec<u64> = holders.iter().filter(|(_, s)| *s == winner).map(|(n, _)| *n).collect();
            let paid: u64 = winners.iter().map(|n| compute_payout(*n, payout_pool, shares[winner])).sum();

            let out = paid + protocol_owed + creator_owed;
            prop_assert!(out <= gross_in, "paid out {out} of only {gross_in}");
            let dust = gross_in - out;
            prop_assert!(dust <= winners.len() as u64 + 1, "{dust} atoms unaccounted for with {} winners", winners.len());
            // Winners share the pot minus the rake - never more than the pot.
            prop_assert!(paid <= payout_pool);
        }

        // Cancelling: everyone is refunded from the whole vault, fees included, pro rata over
        // every share on both sides. Never more than the vault, at most dust left.
        #[test]
        fn refunds_never_exceed_the_vault_and_leave_only_dust(
            bets in proptest::collection::vec((0usize..2, 1_000_000u64..500_000_000), 1..20),
            fee_bps in 0u16..=crate::constants::MAX_FEE_PRIMARY_BPS,
        ) {
            use crate::amm::{curve::{quote_buy, priced_shares, symmetric_seed, virtual_liquidity_per_side}, quote::fee_of};
            use crate::constants::MIN_SEED;

            let seed_each = symmetric_seed(MIN_SEED, 2);
            let v = virtual_liquidity_per_side(seed_each, 30).unwrap();
            let mut shares = [seed_each, seed_each];
            let mut vault = MIN_SEED;
            let mut holders: Vec<u64> = vec![seed_each, seed_each];
            for (side, deposit) in bets {
                let fee = fee_of(deposit, fee_bps);
                let priced = priced_shares(&shares, v).unwrap();
                let Some(q) = quote_buy(&priced, side, deposit - fee) else { continue };
                if q.price_impact_bps > crate::constants::MAX_PRICE_IMPACT_BPS as u32 { continue; }
                shares[side] += q.shares_received;
                vault += deposit; // the vault holds the pot and the fees
                holders.push(q.shares_received);
            }
            let total: u64 = shares.iter().sum();
            let refunded: u64 = holders.iter().map(|n| compute_payout(*n, vault, total)).sum();
            prop_assert!(refunded <= vault);
            prop_assert!(vault - refunded <= holders.len() as u64 + 1);
        }
    }
}
