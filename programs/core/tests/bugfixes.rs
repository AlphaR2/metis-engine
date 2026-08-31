mod common;
// Covers the fixes from the review pass: vault solvency (not exact-equality)
// and the bet-close deadline. Each test proves the specific failure mode the
// fix closes, not just "it still works."
//
// The min_shares_out per-trade slippage tests that used to live here were
// removed along with the field itself — slippage protection is now meant to
// be one global setting (config::state::Config's slippage_limit_bps /
// max_in_bps_of_vault), not wired in yet. See the TODO in
// amm/instructions/place_bet.rs.

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use crate::common::{
    advance_clock_past, authority_keypair, current_clock_ts, funded_keypair,
    ix_initialize_market, ix_place_bet, ix_settle_market, market_pda, position_pda, send, setup,
    setup_with_config, vault_pda,
};
use openstake_core::{
    constants::MARKET_INIT_FEE,
    amm::state::{Market, MarketStatus},
};

#[test]
fn place_bet_after_close_is_rejected() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let nonce = 200u64;

    let (market, _) = market_pda(&creator.pubkey(), nonce);
    let (vault, _) = vault_pda(&market);
    let (creator_position, _) = position_pda(&market, &creator.pubkey());

    let bet_close_ts = current_clock_ts(&svm) + 60;
    send(
        &mut svm,
        ix_initialize_market(&creator.pubkey(), &market, &vault, &creator_position, nonce, 2, bet_close_ts, &config),
        &creator,
        &[],
    )
    .expect("initialize_market should succeed");

    // Betting window has closed — a bet placed now must be rejected, even
    // though the market's status is still Open (nobody has settled it).
    advance_clock_past(&mut svm, bet_close_ts);

    let bettor = funded_keypair(&mut svm, 20 * MARKET_INIT_FEE);
    let (position, _) = position_pda(&market, &bettor.pubkey());
    let result = send(
        &mut svm,
        ix_place_bet(&market, &vault, &position, &bettor.pubkey(), 0, MARKET_INIT_FEE, &config),
        &bettor,
        &[],
    );

    assert!(result.is_err(), "a bet placed after bet_close_ts must be rejected");
}

#[test]
fn settle_market_before_close_is_rejected_even_by_the_real_authority() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let nonce = 203u64;

    let (market, _) = market_pda(&creator.pubkey(), nonce);
    let (vault, _) = vault_pda(&market);
    let (creator_position, _) = position_pda(&market, &creator.pubkey());

    let bet_close_ts = current_clock_ts(&svm) + 3600; // far enough that this test won't reach it
    send(
        &mut svm,
        ix_initialize_market(&creator.pubkey(), &market, &vault, &creator_position, nonce, 2, bet_close_ts, &config),
        &creator,
        &[],
    )
    .expect("initialize_market should succeed");

    let authority = authority_keypair();
    let result = send(&mut svm, ix_settle_market(&market, &vault, &authority.pubkey(), 0), &creator, &[&authority]);

    assert!(result.is_err(), "settle_market must reject settling before bet_close_ts, even by the real authority");

    let market_account = svm.get_account(&market).unwrap();
    let market_state: &Market = from_bytes(&market_account.data);
    assert_eq!(market_state.status, MarketStatus::Open as u8, "a rejected early settle must leave the market untouched");
}

#[test]
fn settle_market_tolerates_a_vault_surplus() {
    // The bug this closes: settle_market used to require vault.lamports()
    // == exactly rent_minimum + total_collected. Anyone can push
    // unsolicited lamports into any account — one stray lamport used to
    // brick this market forever (VaultBalanceMismatch, no recovery path,
    // since nothing else can move vault funds outside claim, which
    // requires Settled). The fix: solvency (>=), not equality.
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let nonce = 204u64;

    let (market, _) = market_pda(&creator.pubkey(), nonce);
    let (vault, _) = vault_pda(&market);
    let (creator_position, _) = position_pda(&market, &creator.pubkey());

    let bet_close_ts = current_clock_ts(&svm) + 60;
    send(
        &mut svm,
        ix_initialize_market(&creator.pubkey(), &market, &vault, &creator_position, nonce, 2, bet_close_ts, &config),
        &creator,
        &[],
    )
    .expect("initialize_market should succeed");

    // Simulate an unsolicited deposit landing in the vault — a plain
    // lamport transfer from a funded wallet, no program instruction
    // involved, exactly the "anyone can push lamports into any account"
    // scenario the fix is about.
    let stray_depositor = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let transfer_ix = solana_system_interface::instruction::transfer(&stray_depositor.pubkey(), &vault, 1);
    send(&mut svm, transfer_ix, &stray_depositor, &[]).expect("a plain lamport transfer into the vault should succeed");

    advance_clock_past(&mut svm, bet_close_ts);
    let authority = authority_keypair();
    let result = send(&mut svm, ix_settle_market(&market, &vault, &authority.pubkey(), 0), &creator, &[&authority]);

    assert!(
        result.is_ok(),
        "settle_market must still succeed with a vault surplus — an exact-equality check would brick this market forever"
    );
}
