mod common;
// Betting against the curve: shares minted, money moved, and the guards that
// stop a bet going through.
//
// To see the printed output:
//   cargo test -p openstake-core --test place_bet -- --nocapture --test-threads=1

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use crate::common::*;
use openstake_core::{amm::state::Market, constants::MARKET_INIT_FEE};

fn usd(raw: u64) -> String {
    format!("${:.2}", raw as f64 / 1e6)
}

fn sh(raw: u64) -> String {
    format!("{:.2}", raw as f64 / 1e6)
}

// The core of a bet: money goes into the vault, shares come out as real SPL
// tokens, and the number minted matches what the curve said to the unit.
#[test]
fn bettor_gets_tokens_and_the_vault_gets_the_money() {
    let mut svm = setup();
    let m = open_market_default(&mut svm, 10);

    let (bettor, quote_ata) = funded_bettor(&mut svm, 50 * MARKET_INIT_FEE);
    let deposit = 20 * MARKET_INIT_FEE;

    let vault_before = token_balance(&svm, &m.vault);
    let wallet_before = token_balance(&svm, &quote_ata);
    let shares_before = {
        let a = svm.get_account(&m.market).unwrap();
        let s: &Market = from_bytes(&a.data);
        s.outcome_shares[0]
    };

    send(
        &mut svm,
        ix_place_bet(
            &m.market,
            &m.vault,
            &bettor.pubkey(),
            &quote_ata,
            0,
            deposit,
            &m.config,
        ),
        &bettor,
        &[],
    )
    .expect("place_bet should succeed");

    let market_account = svm.get_account(&m.market).unwrap();
    let market_state: &Market = from_bytes(&market_account.data);
    let minted = market_state.outcome_shares[0] - shares_before;
    let held = token_balance(&svm, &outcome_ata(&m.market, &bettor.pubkey(), 0));

    println!();
    println!("bet {} on outcome 0", usd(deposit));
    println!(
        "  bettor wallet: {} -> {}",
        usd(wallet_before),
        usd(token_balance(&svm, &quote_ata))
    );
    println!(
        "  vault:         {} -> {}",
        usd(vault_before),
        usd(token_balance(&svm, &m.vault))
    );
    println!("  curve minted:  {} shares", sh(minted));
    println!("  bettor holds:  {} shares  (must match)", sh(held));
    println!("  total_collected: {}", usd(market_state.total_collected));
    println!();

    assert_eq!(token_balance(&svm, &m.vault), vault_before + deposit);
    assert_eq!(token_balance(&svm, &quote_ata), wallet_before - deposit);
    assert!(minted > 0, "a real deposit must mint shares");
    assert_eq!(
        held, minted,
        "the bettor's token balance must match the curve's share delta"
    );

    // total_collected and the vault must never drift apart - settlement's
    // solvency check compares them directly.
    assert_eq!(
        market_state.total_collected,
        token_balance(&svm, &m.vault),
        "total_collected and the vault balance must never diverge"
    );
}

// The mint's supply and the curve's share count are two records of the same
// thing. If they ever drift, claim divides by one while people hold the other.
#[test]
fn mint_supply_tracks_the_curve() {
    let mut svm = setup();
    let m = open_market_default(&mut svm, 11);

    println!();
    for i in 0..3u64 {
        let (bettor, quote_ata) = funded_bettor(&mut svm, 50 * MARKET_INIT_FEE);
        let side = (i % 2) as u8;
        let amount = (i + 1) * MARKET_INIT_FEE;
        send(
            &mut svm,
            ix_place_bet(
                &m.market,
                &m.vault,
                &bettor.pubkey(),
                &quote_ata,
                side,
                amount,
                &m.config,
            ),
            &bettor,
            &[],
        )
        .unwrap_or_else(|e| panic!("bet {i} should succeed: {e:?}"));
        println!("bet {i}: {} on outcome {side}", usd(amount));
    }

    let market_account = svm.get_account(&m.market).unwrap();
    let market_state: &Market = from_bytes(&market_account.data);

    for i in 0..2u8 {
        let (mint, _) = outcome_mint_pda(&m.market, i);
        let supply = mint_supply(&svm, &mint);
        let curve = market_state.outcome_shares[i as usize];
        println!(
            "outcome {i}: mint supply {}  curve says {}",
            sh(supply),
            sh(curve)
        );
        assert_eq!(
            supply, curve,
            "outcome {i}: mint supply drifted from the curve"
        );
    }
    println!();
}

// A second bet on the same outcome lands in the same token account.
// CreateIdempotent has to cope with the account already existing, otherwise
// nobody could ever bet twice.
#[test]
fn betting_twice_tops_up_the_same_token_account() {
    let mut svm = setup();
    let m = open_market_default(&mut svm, 12);

    let (bettor, quote_ata) = funded_bettor(&mut svm, 50 * MARKET_INIT_FEE);
    let position = outcome_ata(&m.market, &bettor.pubkey(), 0);

    send(
        &mut svm,
        ix_place_bet(
            &m.market,
            &m.vault,
            &bettor.pubkey(),
            &quote_ata,
            0,
            5 * MARKET_INIT_FEE,
            &m.config,
        ),
        &bettor,
        &[],
    )
    .expect("first bet");
    let after_first = token_balance(&svm, &position);

    send(
        &mut svm,
        ix_place_bet(
            &m.market,
            &m.vault,
            &bettor.pubkey(),
            &quote_ata,
            0,
            5 * MARKET_INIT_FEE,
            &m.config,
        ),
        &bettor,
        &[],
    )
    .expect("second bet");
    let after_second = token_balance(&svm, &position);

    println!();
    println!("same account {position}");
    println!("  after first bet:  {} shares", sh(after_first));
    println!("  after second bet: {} shares", sh(after_second));
    println!();

    assert!(
        after_second > after_first,
        "a second bet must add to the same balance"
    );
}

// Nothing stops someone hedging. The two outcomes are separate mints, so both
// positions sit side by side without interfering.
#[test]
fn one_bettor_can_hold_both_sides() {
    let mut svm = setup();
    let m = open_market_default(&mut svm, 13);

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
    .expect("yes bet");
    let yes_held = token_balance(&svm, &outcome_ata(&m.market, &bettor.pubkey(), 0));

    send(
        &mut svm,
        ix_place_bet(
            &m.market,
            &m.vault,
            &bettor.pubkey(),
            &quote_ata,
            1,
            5 * MARKET_INIT_FEE,
            &m.config,
        ),
        &bettor,
        &[],
    )
    .expect("no bet");

    let yes_after = token_balance(&svm, &outcome_ata(&m.market, &bettor.pubkey(), 0));
    let no_after = token_balance(&svm, &outcome_ata(&m.market, &bettor.pubkey(), 1));

    println!();
    println!("one bettor, both sides:");
    println!(
        "  outcome 0: {} shares  (unchanged by the second bet)",
        sh(yes_after)
    );
    println!("  outcome 1: {} shares", sh(no_after));
    println!();

    assert_eq!(
        yes_after, yes_held,
        "betting the other side must not touch the first position"
    );
    assert!(no_after > 0);
}

// The whole point of the curve: each successive dollar buys fewer shares. That
// gap is slippage, and it's what makes moving the price expensive rather than
// free. A flat-price market would show identical numbers on every row.
#[test]
fn price_moves_against_the_buyer() {
    let mut svm = setup();
    let m = open_market_default(&mut svm, 14);

    let deposit = 20 * MARKET_INIT_FEE;
    let mut previous = u64::MAX;

    println!();
    println!("three identical {} bets on the same outcome:", usd(deposit));

    for i in 0..3 {
        let (bettor, quote_ata) = funded_bettor(&mut svm, 50 * MARKET_INIT_FEE);
        send(
            &mut svm,
            ix_place_bet(
                &m.market,
                &m.vault,
                &bettor.pubkey(),
                &quote_ata,
                0,
                deposit,
                &m.config,
            ),
            &bettor,
            &[],
        )
        .expect("bet should succeed");

        let got = token_balance(&svm, &outcome_ata(&m.market, &bettor.pubkey(), 0));
        println!("  bettor {}: got {} shares", i + 1, sh(got));

        assert!(
            got < previous,
            "bet {i}: the same deposit must buy fewer shares as the pool fills up \
             (got {got}, previous {previous})"
        );
        previous = got;
    }
    println!();
}

// Betting stops well before kickoff. Once the event is under way, anyone
// watching it knows more than the chain does, so trading has to be shut - the
// AMM would otherwise happily quote a price it has no business quoting.
#[test]
fn bet_after_primary_close_is_rejected() {
    let mut svm = setup();
    let close_ts = default_close_ts(&svm);
    let m = open_market(&mut svm, 15, close_ts);

    let primary_end = primary_end_of(close_ts);
    advance_clock_past(&mut svm, primary_end);

    let (bettor, quote_ata) = funded_bettor(&mut svm, 50 * MARKET_INIT_FEE);
    let result = send(
        &mut svm,
        ix_place_bet(
            &m.market,
            &m.vault,
            &bettor.pubkey(),
            &quote_ata,
            0,
            MARKET_INIT_FEE,
            &m.config,
        ),
        &bettor,
        &[],
    );

    println!();
    println!("betting closed at: {primary_end}");
    println!("clock now:         {}", current_clock_ts(&svm));
    println!("kickoff is still:  {close_ts} (not yet reached)");
    println!(
        "bet -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(result.is_err(), "a bet after primary_end must be rejected");
}

#[test]
fn zero_deposit_is_rejected() {
    let mut svm = setup();
    let m = open_market_default(&mut svm, 16);
    let (bettor, quote_ata) = funded_bettor(&mut svm, 50 * MARKET_INIT_FEE);

    let result = send(
        &mut svm,
        ix_place_bet(
            &m.market,
            &m.vault,
            &bettor.pubkey(),
            &quote_ata,
            0,
            0,
            &m.config,
        ),
        &bettor,
        &[],
    );

    println!();
    println!(
        "deposit of 0 -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(result.is_err(), "a zero deposit must be rejected");
}

// Share amounts round down, so a tiny deposit into a deep pool computes to
// zero shares. Taking the money anyway would be theft, so the deposit has to
// be refused outright.
#[test]
fn dust_deposit_that_buys_nothing_is_rejected() {
    let mut svm = setup();
    let m = open_market_default(&mut svm, 17);

    // Make the pool deep enough that 1 base unit rounds to nothing.
    let (whale, whale_ata) = funded_bettor(&mut svm, 100_000 * MARKET_INIT_FEE);
    send(
        &mut svm,
        ix_place_bet(
            &m.market,
            &m.vault,
            &whale.pubkey(),
            &whale_ata,
            0,
            50_000 * MARKET_INIT_FEE,
            &m.config,
        ),
        &whale,
        &[],
    )
    .expect("whale bet");

    let (bettor, quote_ata) = funded_bettor(&mut svm, MARKET_INIT_FEE);
    let before = token_balance(&svm, &quote_ata);

    let result = send(
        &mut svm,
        ix_place_bet(
            &m.market,
            &m.vault,
            &bettor.pubkey(),
            &quote_ata,
            1,
            1,
            &m.config,
        ),
        &bettor,
        &[],
    );

    println!();
    println!("pool is now {} deep", usd(token_balance(&svm, &m.vault)));
    println!(
        "depositing 1 base unit (0.000001 USDC) -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!(
        "bettor wallet unchanged: {}",
        token_balance(&svm, &quote_ata) == before
    );
    println!();

    assert!(
        result.is_err(),
        "a deposit that mints zero shares must be rejected"
    );
    assert_eq!(
        token_balance(&svm, &quote_ata),
        before,
        "a rejected bet must not take the bettor's money"
    );
}

// max_in_bps_of_vault caps one trade as a fraction of the pool, so a whale
// can't swing a thin market in a single shot. It's measured against the
// vault's current balance, so the allowed size grows as the market deepens.
#[test]
fn oversized_bet_is_rejected_when_max_in_is_set() {
    let mut svm = setup();
    let config = setup_with_config_limits(&mut svm, 0, 5_000); // 50% of vault
    let close_ts = default_close_ts(&svm);
    let m = open_market_with_config(&mut svm, 18, close_ts, config);

    let (bettor, quote_ata) = funded_bettor(&mut svm, 1_000 * MARKET_INIT_FEE);

    let vault = token_balance(&svm, &m.vault);
    let cap = vault / 2;

    println!();
    println!("max_in set to 50% of the vault");
    println!("  vault holds: {}", usd(vault));
    println!("  cap is:      {}", usd(cap));

    let over_cap = MARKET_INIT_FEE;
    let result = send(
        &mut svm,
        ix_place_bet(
            &m.market,
            &m.vault,
            &bettor.pubkey(),
            &quote_ata,
            0,
            over_cap,
            &m.config,
        ),
        &bettor,
        &[],
    );
    println!(
        "  bet {} (over cap)  -> {}",
        usd(over_cap),
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    assert!(
        result.is_err(),
        "a bet over the max_in cap must be rejected"
    );

    let under_cap = MARKET_INIT_FEE / 2 - 1;
    send(
        &mut svm,
        ix_place_bet(
            &m.market,
            &m.vault,
            &bettor.pubkey(),
            &quote_ata,
            0,
            under_cap,
            &m.config,
        ),
        &bettor,
        &[],
    )
    .expect("a bet under the cap should succeed");
    println!("  bet {} (under cap) -> accepted", usd(under_cap));
    println!();
}
