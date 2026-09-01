mod common;
// A visible run of 35 bettors through the real program, printing every number
// that moves. Not a strict assertion suite - it's the "what does this actually
// look like in practice" view. The pass/fail solvency proof is its own test in
// scenario_bulk_solvency.rs.
//
// Run with:
//   cargo test --test scenario_bulk_bettors -- --nocapture

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use crate::common::*;
use openstake_core::{amm::curve::price, amm::state::Market, constants::MARKET_INIT_FEE};

// USDC and shares are both scaled by 1e6, so divide by this for a human number.
const SCALE: f64 = 1_000_000.0;

fn usd(raw: u64) -> String {
    format!("${:.2}", raw as f64 / SCALE)
}

fn shares(raw: u64) -> String {
    let real = raw as f64 / SCALE;
    if real >= 1_000_000.0 {
        format!("{:.2}M", real / 1_000_000.0)
    } else if real >= 1_000.0 {
        format!("{:.2}K", real / 1_000.0)
    } else {
        format!("{real:.2}")
    }
}

#[test]
fn thirty_five_bettors_price_and_slippage() {
    let mut svm = setup();
    let m = open_market_default(&mut svm, 99);

    println!();
    println!(
        "{:<3} {:<2} {:>10} | {:>12} {:>12} | {:>7} {:>7} | {:>12} {:>6} | {:>14}",
        "#",
        "S",
        "paid",
        "pool A",
        "pool B",
        "priceA",
        "priceB",
        "got shares",
        "slip%",
        "collected"
    );
    println!("{}", "-".repeat(110));

    let mut worst_slippage = 0.0f64;
    let mut total_slippage = 0.0f64;

    for i in 0..35u64 {
        let side = (i % 2) as u8;
        let deposit = (1 + (i * 3 + 2) % 7) * MARKET_INIT_FEE;

        // Snapshot before the bet - the left half of the row.
        let (pool_a, pool_b, price_a, price_b) = {
            let a = svm.get_account(&m.market).unwrap();
            let s: &Market = from_bytes(&a.data);
            (
                s.outcome_shares[0],
                s.outcome_shares[1],
                price(&s.outcome_shares, 0),
                price(&s.outcome_shares, 1),
            )
        };

        // What a naive "the price won't move" guess would predict. The gap
        // between this and reality IS the slippage - a flat-price market
        // wouldn't have it.
        let price_here = if side == 0 { price_a } else { price_b };
        let expected = if price_here > 0.0 {
            deposit as f64 / price_here
        } else {
            0.0
        };

        let (bettor, quote_ata) = funded_bettor(&mut svm, deposit + 10 * MARKET_INIT_FEE);
        send(
            &mut svm,
            ix_place_bet(
                &m.market,
                &m.vault,
                &bettor.pubkey(),
                &quote_ata,
                side,
                deposit,
                &m.config,
            ),
            &bettor,
            &[],
        )
        .unwrap_or_else(|e| panic!("bettor {i} (side {side}, {deposit}) should succeed: {e:?}"));

        let got = token_balance(&svm, &outcome_ata(&m.market, &bettor.pubkey(), side));
        let collected = {
            let a = svm.get_account(&m.market).unwrap();
            let s: &Market = from_bytes(&a.data);
            s.total_collected
        };

        let slippage = if expected > 0.0 {
            (expected - got as f64) / expected * 100.0
        } else {
            0.0
        };
        total_slippage += slippage;
        if slippage > worst_slippage {
            worst_slippage = slippage;
        }

        println!(
            "{:<3} {:<2} {:>10} | {:>12} {:>12} | {:>7.4} {:>7.4} | {:>12} {:>5.1}% | {:>14}",
            i + 1,
            if side == 0 { "A" } else { "B" },
            usd(deposit),
            shares(pool_a),
            shares(pool_b),
            price_a,
            price_b,
            shares(got),
            slippage,
            usd(collected)
        );
    }

    let account = svm.get_account(&m.market).unwrap();
    let final_state: &Market = from_bytes(&account.data);
    let price_a = price(&final_state.outcome_shares, 0);
    let price_b = price(&final_state.outcome_shares, 1);

    println!("{}", "-".repeat(110));
    println!(
        "final pool: A={}  B={}  collected={}",
        shares(final_state.outcome_shares[0]),
        shares(final_state.outcome_shares[1]),
        usd(final_state.total_collected)
    );
    // Prices are normalised coordinates, so they don't sum to 1 - their
    // SQUARES do. That's what turns them into something probability-shaped.
    println!(
        "implied odds: A={:.1}%  B={:.1}%",
        price_a * price_a * 100.0,
        price_b * price_b * 100.0
    );
    println!(
        "slippage over 35 bets: average={:.1}%  worst={:.1}%",
        total_slippage / 35.0,
        worst_slippage
    );
    println!();

    // The vault must hold exactly what the market says it collected.
    assert_eq!(
        token_balance(&svm, &m.vault),
        final_state.total_collected,
        "vault balance and total_collected must never diverge"
    );
}
