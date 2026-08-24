// Tier 4-ish: not a strict pass/fail assertion suite, a visible scenario —
// run 35 bettors through the real program, print price/slippage as we go,
// then settle and have everyone claim, proving solvency at the end.
//
// Run with:
//   cargo test scenario_bulk_bettors -- --nocapture

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use super::{funded_keypair, ix_initialize_market, ix_place_bet, market_pda, position_pda, send, setup, vault_pda};
use crate::{
    constants::MARKET_INIT_FEE,
    curve::padded_price,
    state::{Market, Position},
};

#[test]
fn thirty_five_bettors_price_slippage_and_final_solvency() {
    let mut svm = setup();
    let creator = funded_keypair(&mut svm, 50 * MARKET_INIT_FEE);
    let nonce = 99u64;

    let (market, _) = market_pda(&creator.pubkey(), nonce);
    let (vault, _) = vault_pda(&market);
    let (creator_position, _) = position_pda(&market, &creator.pubkey());

    send(
        &mut svm,
        ix_initialize_market(&creator.pubkey(), &market, &vault, &creator_position, nonce, 2),
        &creator,
        &[],
    )
    .expect("initialize_market should succeed");

    println!();
    println!("{:<4} {:<6} {:>10} {:>14} {:>14} {:>10} {:>10}",
        "#", "side", "paid", "price_before", "naive_shares", "got", "slip%");
    println!("{}", "-".repeat(78));

    // 35 bettors, alternating sides, deterministic varied amounts so the
    // run is reproducible — no external rand crate needed for this.
    let mut bettors = Vec::with_capacity(35);
    for i in 0..35u64 {
        let side: u8 = (i % 2) as u8;
        // amounts cycle 1x..7x the base fee, deliberately uneven
        let multiplier = 1 + (i * 3 + 2) % 7;
        let deposit = multiplier * (MARKET_INIT_FEE / 4).max(1);
        bettors.push((side, deposit));
    }

    let mut total_slippage_sum = 0.0f64;
    let mut worst_slippage = 0.0f64;

    for (i, (side, deposit)) in bettors.iter().enumerate() {
        let bettor = funded_keypair(&mut svm, deposit + 5 * MARKET_INIT_FEE);
        let (position, _) = position_pda(&market, &bettor.pubkey());

        // price_before, read from real on-chain state, using the same
        // padded pricing function place_bet itself uses.
        let market_before = svm.get_account(&market).unwrap();
        let market_state_before: &Market = from_bytes(&market_before.data);
        let price_before = padded_price(&market_state_before.outcome_shares, *side as usize);
        let naive_shares = if price_before > 0.0 { *deposit as f64 / price_before } else { 0.0 };

        let ix = ix_place_bet(&market, &vault, &position, &bettor.pubkey(), *side, *deposit);
        send(&mut svm, ix, &bettor, &[])
            .unwrap_or_else(|e| panic!("bettor {i} (side {side}, {deposit}) should succeed: {e:?}"));

        let position_account = svm.get_account(&position).unwrap();
        let position_state: &Position = from_bytes(&position_account.data);
        let got = position_state.outcome_shares[*side as usize];

        let slippage_pct = if naive_shares > 0.0 {
            (naive_shares - got as f64) / naive_shares * 100.0
        } else {
            0.0
        };
        total_slippage_sum += slippage_pct;
        if slippage_pct > worst_slippage {
            worst_slippage = slippage_pct;
        }

        println!(
            "{:<4} {:<6} {:>10} {:>14.4} {:>14.2} {:>10} {:>9.1}%",
            i + 1,
            if *side == 0 { "A" } else { "B" },
            deposit,
            price_before,
            naive_shares,
            got,
            slippage_pct
        );
    }

    let market_final = svm.get_account(&market).unwrap();
    let market_state_final: &Market = from_bytes(&market_final.data);
    let price_a_final = padded_price(&market_state_final.outcome_shares, 0);
    let price_b_final = padded_price(&market_state_final.outcome_shares, 1);

    println!("{}", "-".repeat(78));
    println!(
        "final pool: A_shares={} B_shares={} total_collected={}",
        market_state_final.outcome_shares[0],
        market_state_final.outcome_shares[1],
        market_state_final.total_collected
    );
    println!(
        "final price: A={:.4} ({:.1}%)  B={:.4} ({:.1}%)",
        price_a_final,
        price_a_final * price_a_final * 100.0,
        price_b_final,
        price_b_final * price_b_final * 100.0
    );
    println!(
        "slippage across {} bets: average={:.1}%  worst={:.1}%",
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
