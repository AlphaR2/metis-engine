mod common;
// Market creation: the accounts it builds, the state it writes, and the
// arguments it refuses.
//
// To see the printed output:
//   cargo test -p openstake-core --test initialize_market -- --nocapture --test-threads=1

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use crate::common::*;
use openstake_core::{
    amm::state::{Market, MarketStatus},
    constants::{MARKET_INIT_FEE, PRIMARY_WINDOW_SECS, QUOTE_DECIMALS},
};

fn usd(raw: u64) -> String {
    format!("${:.2}", raw as f64 / 1e6)
}

// Opening a market has to create five things at once: the market record, a
// USDC vault, and one mint per outcome. If any of them is missing or wrongly
// sized, nothing downstream works.
#[test]
fn creates_market_vault_and_both_outcome_mints() {
    let mut svm = setup();
    let close_ts = default_close_ts(&svm);
    let m = open_market(&mut svm, 1, close_ts);

    let market_account = svm.get_account(&m.market).unwrap();
    let market_state: &Market = from_bytes(&market_account.data);

    println!();
    println!("market {}", m.market);
    println!("  status:          {} (Open)", market_state.status);
    println!("  outcomes:        {}", market_state.num_outcomes);
    println!("  kickoff:         {}", market_state.bet_close_ts);
    println!(
        "  betting closes:  {}  ({}h before kickoff)",
        market_state.primary_end,
        PRIMARY_WINDOW_SECS / 3600
    );
    println!("  total_collected: {}", usd(market_state.total_collected));
    println!("  vault balance:   {}", usd(token_balance(&svm, &m.vault)));
    println!(
        "  seeded shares:   [{:.2}, {:.2}]  (equal, so no favourite)",
        market_state.outcome_shares[0] as f64 / 1e6,
        market_state.outcome_shares[1] as f64 / 1e6
    );

    assert_eq!(market_state.status, MarketStatus::Open as u8);
    assert_eq!(market_state.num_outcomes, 2);
    assert_eq!(market_state.bet_close_ts, close_ts);
    assert_eq!(
        market_state.primary_end,
        close_ts - PRIMARY_WINDOW_SECS,
        "betting must close a full primary window before kickoff"
    );
    assert_eq!(market_state.total_collected, MARKET_INIT_FEE);
    assert_eq!(market_state.creator, m.creator.pubkey().to_bytes().into());
    assert_eq!(market_state.vault, m.vault.to_bytes().into());
    assert_eq!(market_state.quote_mint, usdc_mint().to_bytes().into());

    assert!(market_state.outcome_shares[0] > 0);
    assert_eq!(
        market_state.outcome_shares[0], market_state.outcome_shares[1],
        "a fresh market must be balanced"
    );

    // The vault holds exactly the creation stake, and nothing else.
    assert_eq!(
        token_balance(&svm, &m.vault),
        MARKET_INIT_FEE,
        "vault balance must equal total_collected"
    );

    // One real mint per outcome, each already carrying the seed shares. The
    // mint's supply and the curve's share count have to agree from the start,
    // because claim divides by the curve's number at settlement.
    for i in 0..2u8 {
        let (mint, _) = outcome_mint_pda(&m.market, i);
        let mint_account = svm
            .get_account(&mint)
            .unwrap_or_else(|| panic!("outcome mint {i} should exist"));

        println!(
            "  outcome {i} mint {mint}  decimals={} supply={:.2}",
            mint_account.data[44],
            mint_supply(&svm, &mint) as f64 / 1e6
        );

        assert_eq!(mint_account.owner, token_program_id());
        assert_eq!(
            mint_account.data[44], QUOTE_DECIMALS,
            "decimals must match USDC"
        );
        assert_eq!(
            mint_supply(&svm, &mint),
            market_state.outcome_shares[i as usize],
            "mint supply must track the curve's share count for outcome {i}"
        );
    }
    println!();
}

// The creation stake buys a real position - the creator holds outcome tokens
// like any other bettor and gets paid out the same way if they win. It is not
// a fee, and calling it one would misdescribe who is exposed to what.
#[test]
fn creator_receives_the_seed_shares() {
    let mut svm = setup();
    let m = open_market_default(&mut svm, 2);

    let market_account = svm.get_account(&m.market).unwrap();
    let market_state: &Market = from_bytes(&market_account.data);

    println!();
    println!("creator {}", m.creator.pubkey());
    for i in 0..2u8 {
        let held = token_balance(&svm, &outcome_ata(&m.market, &m.creator.pubkey(), i));
        println!(
            "  outcome {i}: holds {:.2} of {:.2} shares in existence",
            held as f64 / 1e6,
            market_state.outcome_shares[i as usize] as f64 / 1e6
        );
        assert_eq!(
            held, market_state.outcome_shares[i as usize],
            "creator should hold every seed share of outcome {i}"
        );
    }
    println!();
}

// The stake has to actually leave the creator's wallet. Recording it in
// total_collected without moving the money would make the market insolvent
// from the moment it opened.
#[test]
fn creation_stake_leaves_the_creators_wallet() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);

    let (creator, creator_quote_ata) = funded_bettor(&mut svm, 100 * MARKET_INIT_FEE);
    let before = token_balance(&svm, &creator_quote_ata);

    let (market, _) = market_pda(&creator.pubkey(), 3);
    let (vault, _) = vault_pda(&market);
    let close_ts = default_close_ts(&svm);
    send(
        &mut svm,
        ix_initialize_market(
            &creator.pubkey(),
            &creator_quote_ata,
            &market,
            &vault,
            3,
            2,
            close_ts,
            &config,
        ),
        &creator,
        &[],
    )
    .expect("initialize_market should succeed");

    let after = token_balance(&svm, &creator_quote_ata);
    println!();
    println!("creator USDC before: {}", usd(before));
    println!(
        "creator USDC after:  {}  (paid {})",
        usd(after),
        usd(before - after)
    );
    println!("vault now holds:     {}", usd(token_balance(&svm, &vault)));
    println!();

    assert_eq!(
        after,
        before - MARKET_INIT_FEE,
        "the stake must actually move out of the creator's USDC account"
    );
}

// Betting closes PRIMARY_WINDOW_SECS before kickoff. A market opened closer
// than that would have its betting window already in the past - the stake
// locked in, nobody able to bet, nothing to do but wait for settlement.
#[test]
fn kickoff_too_soon_is_rejected() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let (creator, creator_quote_ata) = funded_bettor(&mut svm, 100 * MARKET_INIT_FEE);

    let now = current_clock_ts(&svm);
    let too_soon = now + PRIMARY_WINDOW_SECS - 60;
    let (market, _) = market_pda(&creator.pubkey(), 4);
    let (vault, _) = vault_pda(&market);

    println!();
    println!("now:              {now}");
    println!("attempted kickoff:{too_soon}  ({}s away)", too_soon - now);
    println!("required minimum: {}s", PRIMARY_WINDOW_SECS);

    let result = send(
        &mut svm,
        ix_initialize_market(
            &creator.pubkey(),
            &creator_quote_ata,
            &market,
            &vault,
            4,
            2,
            too_soon,
            &config,
        ),
        &creator,
        &[],
    );

    println!(
        "result: {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(
        result.is_err(),
        "a market whose kickoff is inside the primary window must be rejected"
    );
}

// One outcome means a guaranteed winner and nothing to price. Two is the real
// floor for a market, not one.
#[test]
fn single_outcome_market_is_rejected() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let (creator, creator_quote_ata) = funded_bettor(&mut svm, 100 * MARKET_INIT_FEE);

    let (market, _) = market_pda(&creator.pubkey(), 5);
    let (vault, _) = vault_pda(&market);
    let close_ts = default_close_ts(&svm);

    let result = send(
        &mut svm,
        ix_initialize_market(
            &creator.pubkey(),
            &creator_quote_ata,
            &market,
            &vault,
            5,
            1, // only one outcome
            close_ts,
            &config,
        ),
        &creator,
        &[],
    );

    println!();
    println!(
        "num_outcomes=1 -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(result.is_err(), "num_outcomes must be at least 2");
}
