mod common;
// Tier 4: the actual solvency proof at scale. 35 bettors, mixed sides,
// varied amounts, settle, everyone claims (winners and losers), then
// check the one invariant the whole design depends on: the vault never
// pays out more than it collected.
//
// Run with:
//   cargo test scenario_bulk_solvency -- --nocapture

use bytemuck::from_bytes;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signature::Signer};

use crate::common::{
    advance_clock_past, authority_keypair, current_clock_ts, funded_keypair, ix_claim,
    ix_initialize_market, ix_place_bet, ix_settle_market, market_pda, position_pda, send, setup,
    setup_with_config, vault_pda,
};
use openstake_core::{
    constants::MARKET_INIT_FEE,
    {amm::state::Market, settlement::state::Vault},
};

#[test]
fn thirty_five_bettors_settle_and_claim_stay_solvent() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let creator = funded_keypair(&mut svm, 50 * MARKET_INIT_FEE);
    let nonce = 100u64;

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

    // Same 35-bettor pattern as scenario_bulk_bettors.rs, but this time
    // every keypair and position is kept around for the claim pass below.
    // `creator` isn't pushed in yet — it's still needed, un-moved, to sign
    // settle_market below. Keypair deliberately isn't Clone, so it goes in
    // last, right before the claim loop needs it.
    let mut participants: Vec<(Keypair, Pubkey, u8)> = Vec::with_capacity(36);

    for i in 0..35u64 {
        let side: u8 = (i % 2) as u8;
        let multiplier = 1 + (i * 3 + 2) % 7;
        let deposit = multiplier * (MARKET_INIT_FEE / 4).max(1);

        let bettor = funded_keypair(&mut svm, deposit + 5 * MARKET_INIT_FEE);
        let (position, _) = position_pda(&market, &bettor.pubkey());

        let ix = ix_place_bet(&market, &vault, &position, &bettor.pubkey(), side, deposit, &config);
        send(&mut svm, ix, &bettor, &[])
            .unwrap_or_else(|e| panic!("bettor {i} (side {side}, {deposit}) should succeed: {e:?}"));

        participants.push((bettor, position, side));
    }

    let market_before_settle = svm.get_account(&market).unwrap();
    let total_collected = {
        let market_state: &Market = from_bytes(&market_before_settle.data);
        market_state.total_collected
    };

    advance_clock_past(&mut svm, bet_close_ts);
    let authority = authority_keypair();
    let winning_outcome = 0u8; // A wins
    send(&mut svm, ix_settle_market(&market, &vault, &authority.pubkey(), winning_outcome), &creator, &[&authority])
        .expect("settle_market should succeed");

    // creator is only moved now, after its last use as a signer above.
    participants.push((creator, creator_position, u8::MAX)); // side unused for the creator's own claim path below

    // Claim for every single participant — winners and losers alike, same
    // as the real user story: losers still get their rent back.
    let mut total_paid_out: u128 = 0;

    for (keypair, position, _side) in &participants {
        let balance_before = svm.get_account(&keypair.pubkey()).unwrap().lamports as i128;

        let ix = ix_claim(&market, &vault, position, &keypair.pubkey());
        send(&mut svm, ix, keypair, &[])
            .unwrap_or_else(|e| panic!("claim for {} should succeed: {e:?}", keypair.pubkey()));

        let balance_after = svm.get_account(&keypair.pubkey()).unwrap().lamports as i128;
        let received = (balance_after - balance_before).max(0) as u128;
        total_paid_out += received;

        assert!(
            svm.get_account(position).is_none(),
            "position should be closed (drained to zero) after claiming"
        );
    }

    println!();
    println!("participants: {}", participants.len());
    println!("total_collected: {total_collected}");
    println!("total_paid_out (payouts + reclaimed rent): {total_paid_out}");

    // The core invariant: the vault never pays out more of the pot than
    // it actually collected. Rent reclaimed on top of that is real money
    // too, but it came from each position's own rent deposit, not from
    // other bettors' stakes — so we check the vault specifically, not
    // total_paid_out against total_collected (rent inflates that number
    // for a reason unrelated to solvency).
    let vault_account = svm.get_account(&vault).unwrap();
    let rent_minimum = svm.minimum_balance_for_rent_exemption(core::mem::size_of::<Vault>());

    let dust = vault_account.lamports.saturating_sub(rent_minimum);
    println!("vault lamports remaining: {}", vault_account.lamports);
    println!("vault rent-exempt minimum: {rent_minimum}");
    println!("dust left unclaimed (floor-rounding, design doc §8): {dust}");
    println!();

    // Floor-division payouts always round toward the pool, never toward
    // the bettor — a few lamports of dust across 36 claims is expected,
    // not a bug. What actually matters: the vault never goes NEGATIVE
    // relative to rent (couldn't happen, checked_sub would've panicked
    // the instruction first) and dust stays small, not proportional to
    // the amounts moved.
    assert!(
        vault_account.lamports >= rent_minimum,
        "vault must never drop below its own rent-exempt minimum"
    );
    assert!(
        dust < participants.len() as u64,
        "dust should be at most a handful of lamports (one per claim, worst case), \
         not something scaling with the amounts actually paid out — got {dust}"
    );
}
