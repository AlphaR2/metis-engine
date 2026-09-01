mod common;
// Cashing in. Winners burn their shares for a slice of the pool; losers get
// nothing. Includes the case most likely to break silently: several people
// claiming one after another.
//
// To see the printed output:
//   cargo test -p openstake-core --test claim -- --nocapture --test-threads=1

use solana_sdk::{pubkey::Pubkey, signature::Keypair, signature::Signer};

use crate::common::*;
use openstake_core::constants::MARKET_INIT_FEE;

fn usd(raw: u64) -> String {
    format!("${:.2}", raw as f64 / 1e6)
}

fn sh(raw: u64) -> String {
    format!("{:.2}", raw as f64 / 1e6)
}

// Opens a market, runs the given bets, settles on `winner`, and hands back the
// bettors so the test can claim for them.
fn market_with_bets(
    svm: &mut litesvm::LiteSVM,
    nonce: u64,
    bets: &[(u8, u64)],
    winner: u8,
) -> (TestMarket, Vec<(Keypair, Pubkey)>) {
    let close_ts = default_close_ts(svm);
    let m = open_market(svm, nonce, close_ts);

    let mut bettors = Vec::new();
    for (i, (side, amount)) in bets.iter().enumerate() {
        let (kp, quote_ata) = funded_bettor(svm, 1_000 * MARKET_INIT_FEE);
        send(
            svm,
            ix_place_bet(
                &m.market,
                &m.vault,
                &kp.pubkey(),
                &quote_ata,
                *side,
                *amount,
                &m.config,
            ),
            &kp,
            &[],
        )
        .unwrap_or_else(|e| panic!("bet {i} should succeed: {e:?}"));
        bettors.push((kp, quote_ata));
    }

    advance_clock_past(svm, close_ts);
    let authority = authority_keypair();
    send(
        svm,
        ix_settle_market(&m.market, &m.vault, &authority.pubkey(), winner),
        &m.creator,
        &[&authority],
    )
    .expect("settle_market should succeed");

    (m, bettors)
}

// The happy path: shares are burned, USDC comes back. Burning is what makes
// the claim permanent - the tokens stop existing rather than being flagged.
#[test]
fn a_winner_burns_their_shares_and_gets_paid() {
    let mut svm = setup();
    let (m, bettors) = market_with_bets(&mut svm, 30, &[(0, 20 * MARKET_INIT_FEE)], 0);
    let (bettor, quote_ata) = &bettors[0];

    let shares_before = token_balance(&svm, &outcome_ata(&m.market, &bettor.pubkey(), 0));
    let usdc_before = token_balance(&svm, quote_ata);
    let pool = token_balance(&svm, &m.vault);
    assert!(shares_before > 0);

    send(
        &mut svm,
        ix_claim(&m.market, &m.vault, &bettor.pubkey(), quote_ata, 0),
        bettor,
        &[],
    )
    .expect("claim should succeed");

    let usdc_after = token_balance(&svm, quote_ata);

    println!();
    println!("pool at settlement: {}", usd(pool));
    println!("bettor held:        {} winning shares", sh(shares_before));
    println!("after claiming:");
    println!(
        "  shares:  {} (burned)",
        sh(token_balance(
            &svm,
            &outcome_ata(&m.market, &bettor.pubkey(), 0)
        ))
    );
    println!(
        "  USDC:    {} -> {}  (paid {})",
        usd(usdc_before),
        usd(usdc_after),
        usd(usdc_after - usdc_before)
    );
    println!("  vault:   {}", usd(token_balance(&svm, &m.vault)));
    println!();

    assert_eq!(
        token_balance(&svm, &outcome_ata(&m.market, &bettor.pubkey(), 0)),
        0,
        "winning shares must be burned"
    );
    assert!(usdc_after > usdc_before, "a winner must be paid");
}

// Burning is what makes claiming once-only: the second attempt finds a zero
// balance and stops. No separate `claimed` flag to get out of step.
#[test]
fn claiming_twice_is_rejected() {
    let mut svm = setup();
    let (m, bettors) = market_with_bets(&mut svm, 31, &[(0, 20 * MARKET_INIT_FEE)], 0);
    let (bettor, quote_ata) = &bettors[0];

    send(
        &mut svm,
        ix_claim(&m.market, &m.vault, &bettor.pubkey(), quote_ata, 0),
        bettor,
        &[],
    )
    .expect("first claim");

    let after_first = token_balance(&svm, quote_ata);

    let result = send(
        &mut svm,
        ix_claim(&m.market, &m.vault, &bettor.pubkey(), quote_ata, 0),
        bettor,
        &[],
    );

    println!();
    println!("first claim:  accepted, paid out");
    println!(
        "second claim: {}",
        if result.is_err() {
            "rejected (no shares left to burn)"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!(
        "balance unchanged: {}",
        token_balance(&svm, quote_ata) == after_first
    );
    println!();

    assert!(result.is_err(), "a second claim must be rejected");
}

// Nobody gets paid before the result is in. Until then the pool has no owner.
#[test]
fn claiming_before_settlement_is_rejected() {
    let mut svm = setup();
    let m = open_market_default(&mut svm, 32);

    let (bettor, quote_ata) = funded_bettor(&mut svm, 50 * MARKET_INIT_FEE);
    send(
        &mut svm,
        ix_place_bet(
            &m.market,
            &m.vault,
            &bettor.pubkey(),
            &quote_ata,
            0,
            10 * MARKET_INIT_FEE,
            &m.config,
        ),
        &bettor,
        &[],
    )
    .expect("bet");

    let result = send(
        &mut svm,
        ix_claim(&m.market, &m.vault, &bettor.pubkey(), &quote_ata, 0),
        &bettor,
        &[],
    );

    println!();
    println!(
        "market not yet settled; claim -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(
        result.is_err(),
        "claiming an unsettled market must be rejected"
    );
}

// The losing side's tokens are worthless. A loser holds none of the winning
// mint, so there's nothing to burn and the claim fails outright rather than
// quietly paying zero and leaving them thinking it worked.
#[test]
fn a_loser_gets_nothing() {
    let mut svm = setup();
    let (m, bettors) = market_with_bets(
        &mut svm,
        33,
        &[(0, 20 * MARKET_INIT_FEE), (1, 20 * MARKET_INIT_FEE)],
        0, // outcome 0 wins, so bettor 1 lost
    );
    let (loser, loser_quote) = &bettors[1];

    let before = token_balance(&svm, loser_quote);
    let losing_shares = token_balance(&svm, &outcome_ata(&m.market, &loser.pubkey(), 1));
    let winning_shares = token_balance(&svm, &outcome_ata(&m.market, &loser.pubkey(), 0));

    let result = send(
        &mut svm,
        ix_claim(&m.market, &m.vault, &loser.pubkey(), loser_quote, 0),
        loser,
        &[],
    );

    println!();
    println!("winning outcome was 0");
    println!(
        "loser holds {} shares of outcome 1 (worthless)",
        sh(losing_shares)
    );
    println!(
        "loser holds {} shares of outcome 0 (the winner)",
        sh(winning_shares)
    );
    println!(
        "claim -> {}",
        if result.is_err() {
            "rejected, nothing to burn"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!(
        "USDC unchanged: {}",
        token_balance(&svm, loser_quote) == before
    );
    println!();

    assert!(result.is_err(), "a loser has no winning shares to claim");
    assert_eq!(
        token_balance(&svm, loser_quote),
        before,
        "a loser's balance must not change"
    );
}

// The bug this guards against: each claim burns tokens, so the winning mint's
// supply shrinks as people cash in. If the payout divided by the LIVE supply
// instead of the snapshot taken at settlement, each successive claimer would
// get a bigger slice than the last until the vault ran dry.
//
// Three winners of different sizes, claiming one after another - the value per
// share must come out identical for all of them.
#[test]
fn every_winner_gets_the_same_value_per_share() {
    let mut svm = setup();
    let (m, bettors) = market_with_bets(
        &mut svm,
        34,
        &[
            (0, 10 * MARKET_INIT_FEE),
            (0, 25 * MARKET_INIT_FEE),
            (0, 40 * MARKET_INIT_FEE),
            (1, 30 * MARKET_INIT_FEE), // a loser, so the pool has real money in it
        ],
        0,
    );

    let pool = token_balance(&svm, &m.vault);
    println!();
    println!("pool at settlement: {}", usd(pool));
    println!("three winners claiming in sequence:");

    let mut per_share_values = Vec::new();

    for (i, (bettor, quote_ata)) in bettors.iter().take(3).enumerate() {
        let shares = token_balance(&svm, &outcome_ata(&m.market, &bettor.pubkey(), 0));
        let before = token_balance(&svm, quote_ata);

        send(
            &mut svm,
            ix_claim(&m.market, &m.vault, &bettor.pubkey(), quote_ata, 0),
            bettor,
            &[],
        )
        .unwrap_or_else(|e| panic!("claim {i} should succeed: {e:?}"));

        let paid = token_balance(&svm, quote_ata) - before;
        assert!(paid > 0, "winner {i} should be paid");

        // Scaled so integer division doesn't hide small differences.
        let per_share = (paid as u128) * 1_000_000 / (shares as u128);
        println!(
            "  winner {}: {} shares -> {}   ({:.6} per share, vault left {})",
            i + 1,
            sh(shares),
            usd(paid),
            per_share as f64 / 1e6,
            usd(token_balance(&svm, &m.vault))
        );
        per_share_values.push(per_share);
    }
    println!();

    let first = per_share_values[0];
    for (i, v) in per_share_values.iter().enumerate() {
        let diff = if *v > first { v - first } else { first - v };
        assert!(
            diff <= 1,
            "claimer {i} got a different value per share ({v} vs {first}) - \
             the payout is dividing by live supply instead of the settlement snapshot"
        );
    }
}

// The solvency invariant, end to end: whatever the winners take out together
// can never be more than what everyone put in. Floor division means the total
// lands slightly under, leaving dust - the safe direction to be wrong in.
#[test]
fn payouts_never_exceed_the_pool() {
    let mut svm = setup();
    let (m, bettors) = market_with_bets(
        &mut svm,
        35,
        &[
            (0, 7 * MARKET_INIT_FEE),
            (1, 13 * MARKET_INIT_FEE),
            (0, 3 * MARKET_INIT_FEE),
            (1, 21 * MARKET_INIT_FEE),
            (0, 11 * MARKET_INIT_FEE),
        ],
        0,
    );

    let pool = token_balance(&svm, &m.vault);
    let mut paid_out = 0u64;

    println!();
    println!("pool at settlement: {}", usd(pool));

    // The creator holds seed shares on the winning side too, so they claim
    // alongside everyone else.
    let mut claimers: Vec<(&Keypair, &Pubkey)> = vec![(&m.creator, &m.creator_quote_ata)];
    for (kp, ata) in &bettors {
        claimers.push((kp, ata));
    }

    for (n, (kp, quote_ata)) in claimers.into_iter().enumerate() {
        if token_balance(&svm, &outcome_ata(&m.market, &kp.pubkey(), 0)) == 0 {
            println!("  {n}: no winning shares, skipped");
            continue;
        }
        let before = token_balance(&svm, quote_ata);
        send(
            &mut svm,
            ix_claim(&m.market, &m.vault, &kp.pubkey(), quote_ata, 0),
            kp,
            &[],
        )
        .unwrap_or_else(|e| panic!("claim for {} should succeed: {e:?}", kp.pubkey()));
        let paid = token_balance(&svm, quote_ata) - before;
        paid_out += paid;
        println!("  {n}: claimed {}", usd(paid));
    }

    let dust = pool - paid_out;
    println!("total paid out: {}", usd(paid_out));
    println!("dust remaining: {dust} base units ({})", usd(dust));
    println!();

    assert!(
        paid_out <= pool,
        "paid out {paid_out} from a pool of {pool} - the vault promised more than it held"
    );
    assert!(
        dust < 100,
        "dust of {dust} is too large - rounding should leave only a few units"
    );
    assert_eq!(
        token_balance(&svm, &m.vault),
        dust,
        "whatever wasn't paid out must still be sitting in the vault"
    );
}
