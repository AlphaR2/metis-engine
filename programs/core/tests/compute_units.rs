// Every instruction's real compute cost, measured on the full lifecycle. Solana's default limit
// is 200,000 units per instruction; anything that creeps past 150,000 fails here long before it
// fails for a user. The numbers are printed so a change that makes something dearer is visible.

mod common;

use common::*;
use metis_core::{amm::condition::HOME_WIN, config::state::ConfigDials, constants::MIN_DISPUTE_SECS};
use solana_sdk::signature::Signer;

const BUDGET: u64 = 150_000;

fn cost(label: &str, units: u64) {
    println!("{label:<24} {units:>7} CU");
    assert!(units < BUDGET, "{label} used {units} compute units, over the {BUDGET} guard (limit 200,000)");
}

#[test]
fn every_instruction_stays_well_inside_the_compute_limit() {
    let mut svm = setup();
    set_clock(&mut svm, 1_000_000);
    let have_manifest = load_manifest(&mut svm);
    let config = setup_with_dials(&mut svm, ConfigDials { fee_bps: 50, rake_bps: 100, dispute_secs: MIN_DISPUTE_SECS, ..default_dials() });
    let kickoff = default_close_ts(&svm);

    // initialize_market: measured directly, not through the helper.
    let (creator, creator_ata) = funded_bettor(&mut svm, 100_000_000_000);
    let oracle = oracle_account(&mut svm, kickoff);
    let (market, _) = market_pda(&creator.pubkey(), 0);
    let vault = vault_pda(&market).0;
    let ix = ix_initialize_market(&creator.pubkey(), &creator_ata, &market, &vault, 0, 2, HOME_WIN, 0, &config, &oracle);
    cost("initialize_market", send_with_meta(&mut svm, ix, &creator, &[]).unwrap().compute_units_consumed);

    cost(
        "set_metadata",
        send_with_meta(&mut svm, ix_set_metadata(&creator.pubkey(), &market, &Text::sample()), &creator, &[]).unwrap().compute_units_consumed,
    );

    let m = TestMarket { creator, creator_quote_ata: creator_ata, market, vault, config, oracle, bet_close_ts: kickoff };
    let (alice, alice_ata) = funded_bettor(&mut svm, 1_000_000_000);
    let ix = ix_place_bet(&m.market, &m.vault, &alice.pubkey(), &alice_ata, 0, 40_000_000, 0, &m.config);
    cost("place_bet (first)", send_with_meta(&mut svm, ix, &alice, &[]).unwrap().compute_units_consumed);
    let ix = ix_place_bet(&m.market, &m.vault, &alice.pubkey(), &alice_ata, 1, 20_000_000, 0, &m.config);
    cost("place_bet (other side)", send_with_meta(&mut svm, ix, &alice, &[]).unwrap().compute_units_consumed);

    if have_manifest {
        let primary_end = read_market(&svm, &m.market).primary_end;
        set_clock(&mut svm, primary_end);
        let caller = funded_operator(&mut svm, 5_000_000_000);
        cost(
            "close_primary",
            send_with_meta(&mut svm, ix_close_primary(&caller.pubkey(), &m.market, &m.config), &caller, &[]).unwrap().compute_units_consumed,
        );
    }

    let at = kickoff + 7_000;
    finalize_oracle(&mut svm, &m.oracle, 2, 0, at);
    set_clock(&mut svm, at + MIN_DISPUTE_SECS as i64);
    let anyone = funded_operator(&mut svm, 1_000_000_000);
    cost(
        "settle_market",
        send_with_meta(&mut svm, ix_settle_market(&m.market, &m.vault, &m.oracle, &m.config, &anyone.pubkey()), &anyone, &[]).unwrap().compute_units_consumed,
    );

    let t_ata = treasury_ata(&mut svm);
    cost(
        "sweep_fees (protocol)",
        send_with_meta(&mut svm, ix_sweep_fees(&m.market, &m.vault, &t_ata, &m.config, 0, &anyone.pubkey()), &anyone, &[]).unwrap().compute_units_consumed,
    );
    cost(
        "sweep_fees (creator)",
        send_with_meta(&mut svm, ix_sweep_fees(&m.market, &m.vault, &m.creator_quote_ata, &m.config, 1, &anyone.pubkey()), &anyone, &[]).unwrap().compute_units_consumed,
    );
    cost(
        "claim",
        send_with_meta(&mut svm, ix_claim(&m.market, &m.vault, &alice.pubkey(), &alice_ata, 0), &alice, &[]).unwrap().compute_units_consumed,
    );

    let resolved = read_market(&svm, &m.market).resolved_at;
    set_clock(&mut svm, resolved + metis_core::constants::CLAIM_WINDOW_SECS);
    let recipient = funded_operator(&mut svm, 1_000_000_000);
    cost(
        "close_market",
        send_with_meta(&mut svm, ix_close_market(&m.market, &m.vault, &t_ata, &recipient.pubkey(), &m.config), &recipient, &[]).unwrap().compute_units_consumed,
    );
}

#[test]
fn forfeit_and_cancel_stay_inside_the_compute_limit() {
    let mut svm = setup();
    set_clock(&mut svm, 1_000_000);
    let dials = ConfigDials { fee_bps: 50, rake_bps: 100, dispute_secs: MIN_DISPUTE_SECS, ..default_dials() };
    let config = setup_with_dials(&mut svm, dials);
    let kickoff = default_close_ts(&svm);
    let m = open_market_with_config(&mut svm, 0, kickoff, config);
    let (bettor, ata) = funded_bettor(&mut svm, 100_000_000);
    send(&mut svm, ix_place_bet(&m.market, &m.vault, &bettor.pubkey(), &ata, 0, 40_000_000, 0, &m.config), &bettor, &[]).unwrap();

    // A second market on its own oracle, voided, to measure the cancel.
    let kickoff2 = default_close_ts(&svm) + 10_000;
    let m2 = open_market_with_config(&mut svm, 1, kickoff2, config);
    void_oracle(&mut svm, &m2.oracle, kickoff2 + 1);
    set_clock(&mut svm, kickoff2 + 2);
    let op = funded_operator(&mut svm, 1_000_000_000);
    cost(
        "cancel_market",
        send_with_meta(&mut svm, ix_cancel_market(&m2.market, &m2.oracle, &m2.config, &op.pubkey(), None), &op, &[]).unwrap().compute_units_consumed,
    );

    let at = kickoff + 7_000;
    finalize_oracle(&mut svm, &m.oracle, 1, 0, at);
    set_clock(&mut svm, at + MIN_DISPUTE_SECS as i64);
    send(&mut svm, ix_settle_market(&m.market, &m.vault, &m.oracle, &m.config, &op.pubkey()), &op, &[]).unwrap();
    let resolved = read_market(&svm, &m.market).resolved_at;
    set_clock(&mut svm, resolved + metis_core::constants::CREATOR_CLAIM_SECS);
    let t_ata = treasury_ata(&mut svm);
    let (r0, r1) = (root_signer(0), root_signer(1));
    svm.airdrop(&r0, 1_000_000_000).unwrap();
    cost(
        "forfeit_creator_fees",
        send_as_meta(&mut svm, ix_forfeit_creator_fees(&m.market, &m.vault, &t_ata, &m.config, &r0, &r1), r0).unwrap().compute_units_consumed,
    );
}
