mod common;
// Tier 4-ish: not a strict pass/fail assertion suite, a visible scenario —
// run 35 bettors through the real program, print every number that moves
// as we go, then settle and have everyone claim, proving solvency at the
// end (that part is its own test, scenario_bulk_solvency.rs).
//
// Run with:
//   cargo test scenario_bulk_bettors -- --nocapture

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use crate::common::{funded_keypair, ix_initialize_market, ix_place_bet, market_pda, position_pda, send, setup, setup_with_config, vault_pda};
use openstake_core::{
    constants::MARKET_INIT_FEE,
    amm::curve::price,
    amm::state::{Market, Position},
};

// MARKET_INIT_FEE is denominated in the same base units as every USDC
// figure here (6 decimals, matching real USDC) — divide by this to get
// a human dollar amount for display. Shares are stored in that same
// 1e6-scaled base unit too (see human() below).
const USDC_DECIMALS: f64 = 1_000_000.0;

fn usd(raw: u64) -> String {
    format!("${:.2}", raw as f64 / USDC_DECIMALS)
}

// Shares are stored in the same 1e6-scaled base units as USDC (the curve
// ties deposit amounts directly to share amounts), so a raw value like
// 7,071,067 is really just 7.07 shares — NOT 7.07 million. Descale first,
// same as usd(), then only reach for K/M/B suffixes if the real share
// count itself is large.
fn human(raw: u64) -> String {
    let real = raw as f64 / USDC_DECIMALS;
    human_real(real)
}

fn human_real(real: f64) -> String {
    if real >= 1_000_000_000.0 {
        format!("{:.2}B", real / 1_000_000_000.0)
    } else if real >= 1_000_000.0 {
        format!("{:.2}M", real / 1_000_000.0)
    } else if real >= 1_000.0 {
        format!("{:.2}K", real / 1_000.0)
    } else {
        format!("{:.2}", real)
    }
}

fn human_f(raw: f64) -> String {
    human_real(raw / USDC_DECIMALS)
}

fn shares(n: u64) -> String {
    human(n)
}

fn shares_f(n: f64) -> String {
    human_f(n)
}

#[test]
fn thirty_five_bettors_price_slippage_and_final_solvency() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let creator = funded_keypair(&mut svm, 50 * MARKET_INIT_FEE);
    let nonce = 99u64;

    let (market, _) = market_pda(&creator.pubkey(), nonce);
    let (vault, _) = vault_pda(&market);
    let (creator_position, _) = position_pda(&market, &creator.pubkey());

    send(
        &mut svm,
        ix_initialize_market(&creator.pubkey(), &market, &vault, &creator_position, nonce, 2, crate::common::FAR_FUTURE_CLOSE, &config),
        &creator,
        &[],
    )
    .expect("initialize_market should succeed");

    // 35 bettors, alternating sides, deterministic varied amounts so the
    // run is reproducible — no external rand crate needed for this.
    // Deposits never go below $100, cycling 1x..7x that floor so the sizes
    // stay uneven ($100..$700) instead of all landing on the same amount.
    const MIN_DEPOSIT: u64 = 100 * 1_000_000; // $100 in USDC base units
    let mut bettors = Vec::with_capacity(35);
    for i in 0..35u64 {
        let side: u8 = (i % 2) as u8;
        let multiplier = 1 + (i * 3 + 2) % 7;
        let deposit = multiplier * MIN_DEPOSIT;
        bettors.push((side, deposit));
    }

    println!();
    println!(
        "{:<3} {:<2} {:>10} | {:>20} {:>20} | {:>7} {:>7} | {:>20} {:>20} {:>6} | {:>7} {:>7} | {:>20} {:>20} {:>14}",
        "#", "S", "paid",
        "pool A (pre)", "pool B (pre)",
        "priceA_pre", "priceB_pre",
        "expected sh", "got sh", "slip%",
        "priceA_post", "priceB_post",
        "pool A (post)", "pool B (post)", "total collected"
    );
    println!("{}", "-".repeat(210));

    let mut total_slippage_sum = 0.0f64;
    let mut worst_slippage = 0.0f64;
    let mut worst_slippage_bettor = 0usize;

    for (i, (side, deposit)) in bettors.iter().enumerate() {
        let bettor = funded_keypair(&mut svm, deposit + 5 * MARKET_INIT_FEE);
        let (position, _) = position_pda(&market, &bettor.pubkey());

        // Snapshot the pool BEFORE this bet — every number on the left
        // half of the row comes from this read.
        let market_before = svm.get_account(&market).unwrap();
        let market_state_before: &Market = from_bytes(&market_before.data);
        let pool_a_pre = market_state_before.outcome_shares[0];
        let pool_b_pre = market_state_before.outcome_shares[1];
        let price_a_pre = price(&market_state_before.outcome_shares, 0);
        let price_b_pre = price(&market_state_before.outcome_shares, 1);
        let price_before_this_side = if *side == 0 { price_a_pre } else { price_b_pre };

        // "Expected" = what a naive linear model predicts: payment / price,
        // as if the price stayed fixed for the whole trade. The gap
        // between this and what's actually received IS the slippage —
        // this is the number a flat/CPMM-style market wouldn't have.
        let expected_shares = if price_before_this_side > 0.0 {
            *deposit as f64 / price_before_this_side
        } else {
            0.0
        };

        let ix = ix_place_bet(&market, &vault, &position, &bettor.pubkey(), *side, *deposit, &config);
        send(&mut svm, ix, &bettor, &[])
            .unwrap_or_else(|e| panic!("bettor {i} (side {side}, {deposit}) should succeed: {e:?}"));

        // Snapshot AFTER — every number on the right half of the row.
        let position_account = svm.get_account(&position).unwrap();
        let position_state: &Position = from_bytes(&position_account.data);
        let got_shares = position_state.outcome_shares[*side as usize];

        let market_after = svm.get_account(&market).unwrap();
        let market_state_after: &Market = from_bytes(&market_after.data);
        let pool_a_post = market_state_after.outcome_shares[0];
        let pool_b_post = market_state_after.outcome_shares[1];
        let price_a_post = price(&market_state_after.outcome_shares, 0);
        let price_b_post = price(&market_state_after.outcome_shares, 1);
        let total_collected_after = market_state_after.total_collected;

        let slippage_pct = if expected_shares > 0.0 {
            (expected_shares - got_shares as f64) / expected_shares * 100.0
        } else {
            0.0
        };
        total_slippage_sum += slippage_pct;
        if slippage_pct > worst_slippage {
            worst_slippage = slippage_pct;
            worst_slippage_bettor = i + 1;
        }

        println!(
            "{:<3} {:<2} {:>10} | {:>20} {:>20} | {:>7.4} {:>7.4} | {:>20} {:>20} {:>5.1}% | {:>7.4} {:>7.4} | {:>20} {:>20} {:>14}",
            i + 1,
            if *side == 0 { "A" } else { "B" },
            usd(*deposit),
            shares(pool_a_pre), shares(pool_b_pre),
            price_a_pre, price_b_pre,
            shares_f(expected_shares), shares(got_shares), slippage_pct,
            price_a_post, price_b_post,
            shares(pool_a_post), shares(pool_b_post), usd(total_collected_after)
        );
    }

    let market_final = svm.get_account(&market).unwrap();
    let market_state_final: &Market = from_bytes(&market_final.data);
    let price_a_final = price(&market_state_final.outcome_shares, 0);
    let price_b_final = price(&market_state_final.outcome_shares, 1);

    println!("{}", "-".repeat(210));
    println!(
        "final pool: A={} shares   B={} shares   total collected={}",
        shares(market_state_final.outcome_shares[0]),
        shares(market_state_final.outcome_shares[1]),
        usd(market_state_final.total_collected)
    );
    println!(
        "final price: A={:.4} ({:.1}%)  B={:.4} ({:.1}%)",
        price_a_final,
        price_a_final * price_a_final * 100.0,
        price_b_final,
        price_b_final * price_b_final * 100.0
    );
    println!(
        "slippage across {} bets: average={:.1}%  worst={:.1}% (bettor #{worst_slippage_bettor})",
        bettors.len(),
        total_slippage_sum / bettors.len() as f64,
        worst_slippage
    );
    println!();

    // This test stops at the price/slippage table on purpose — it's the
    // visible "how did 35 people buying in actually play out" view.
    // The settle + everyone-claims + solvency assertion is its own test,
    // in scenario_bulk_solvency.rs, since that's a different claim being
    // proven and deserves its own pass/fail rather than living at the
    // bottom of a println-heavy demonstration.
}
