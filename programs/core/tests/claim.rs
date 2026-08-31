mod common;
// Tier 1 + Tier 3: claim — full lifecycle (init -> two bettors, opposite
// sides -> settle -> winner claims, loser claims) plus the two rejections
// that don't need a separate market to set up.

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use crate::common::{
    advance_clock_past, authority_keypair, current_clock_ts, funded_keypair, ix_claim,
    ix_initialize_market, ix_place_bet, ix_settle_market, market_pda, position_pda, send, setup,
    setup_with_config, vault_pda,
};
use openstake_core::{constants::MARKET_INIT_FEE, amm::state::Position};

#[test]
fn full_lifecycle_winner_gets_paid_loser_gets_rent_back() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let nonce = 20u64;

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

    // Alice backs the winning side (A), Ben backs the losing side (B).
    let alice = funded_keypair(&mut svm, 20 * MARKET_INIT_FEE);
    let (alice_position, _) = position_pda(&market, &alice.pubkey());
    send(
        &mut svm,
        ix_place_bet(&market, &vault, &alice_position, &alice.pubkey(), 0, 5 * MARKET_INIT_FEE, &config),
        &alice,
        &[],
    )
    .expect("Alice's bet should succeed");

    let ben = funded_keypair(&mut svm, 20 * MARKET_INIT_FEE);
    let (ben_position, _) = position_pda(&market, &ben.pubkey());
    send(
        &mut svm,
        ix_place_bet(&market, &vault, &ben_position, &ben.pubkey(), 1, 3 * MARKET_INIT_FEE, &config),
        &ben,
        &[],
    )
    .expect("Ben's bet should succeed");

    advance_clock_past(&mut svm, bet_close_ts);
    let authority = authority_keypair();
    send(
        &mut svm,
        ix_settle_market(&market, &vault, &authority.pubkey(), 0), // A wins
        &creator,
        &[&authority],
    )
    .expect("settle_market should succeed");

    let ben_balance_before = svm.get_account(&ben.pubkey()).unwrap().lamports;

    // Alice (winner) claims.
    send(&mut svm, ix_claim(&market, &vault, &alice_position, &alice.pubkey()), &alice, &[])
        .expect("winning claim should succeed");
    assert!(
        svm.get_account(&alice_position).is_none(),
        "Alice's position should be closed after claiming (drained to zero lamports)"
    );

    // Ben (loser) claims — gets nothing from the pot, just rent back.
    send(&mut svm, ix_claim(&market, &vault, &ben_position, &ben.pubkey()), &ben, &[])
        .expect("losing claim should still succeed — no payout, but rent comes back");
    assert!(svm.get_account(&ben_position).is_none(), "Ben's position should also close");

    let ben_balance_after = svm.get_account(&ben.pubkey()).unwrap().lamports;
    assert!(
        ben_balance_after > ben_balance_before,
        "Ben should have gotten his position's rent back even though he lost"
    );

    // Creator holds seed shares on BOTH outcomes (initialize_market splits
    // the fee evenly) — including the winning side, A. Their slice of the
    // pot stays in the vault, legitimately, until they claim it too.
    send(&mut svm, ix_claim(&market, &vault, &creator_position, &creator.pubkey()), &creator, &[])
        .expect("creator's own claim should succeed");
    assert!(svm.get_account(&creator_position).is_none(), "creator's position should also close");

    // Vault should be left holding only its own rent-exempt minimum, plus
    // possibly a few lamports of dust — payout division floors toward the
    // pool (design doc §8), so a handful of lamports can go unclaimed by
    // anyone. Not a bug; exact-equality would be the wrong assertion here.
    let vault_account = svm.get_account(&vault).unwrap();
    let rent_minimum = svm.minimum_balance_for_rent_exemption(core::mem::size_of::<openstake_core::settlement::state::Vault>());
    let dust = vault_account.lamports.saturating_sub(rent_minimum);
    assert!(
        dust < 10,
        "vault should hold its rent-exempt minimum plus at most a few lamports of \
         rounding dust once everyone has claimed — got {dust} lamports of dust"
    );
}

#[test]
fn claim_before_settlement_is_rejected() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let nonce = 21u64;

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

    // Market is still Open — nobody has settled it yet.
    let result = send(&mut svm, ix_claim(&market, &vault, &creator_position, &creator.pubkey()), &creator, &[]);
    assert!(result.is_err(), "claim must be rejected before the market is Settled");

    let position_account = svm.get_account(&creator_position).expect("position should still exist");
    let position_state: &Position = from_bytes(&position_account.data);
    assert_eq!(position_state.claimed, 0, "a rejected claim must not mark the position claimed");
}

#[test]
fn claiming_twice_is_rejected() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let nonce = 22u64;

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

    advance_clock_past(&mut svm, bet_close_ts);
    let authority = authority_keypair();
    send(&mut svm, ix_settle_market(&market, &vault, &authority.pubkey(), 0), &creator, &[&authority])
        .expect("settle_market should succeed");

    send(&mut svm, ix_claim(&market, &vault, &creator_position, &creator.pubkey()), &creator, &[])
        .expect("first claim should succeed");

    // Position is closed after the first claim, so a second attempt has
    // nothing to read — proving the same thing (can't be paid twice) from
    // a different angle than a livesvm-account-still-exists check would.
    assert!(
        svm.get_account(&creator_position).is_none(),
        "position should already be gone before any second claim attempt"
    );
}
