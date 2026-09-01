mod common;
// The solvency proof at scale. 35 bettors, mixed sides, varied amounts,
// settle, everyone claims - then check the one invariant the whole design
// rests on: the vault never pays out more than it took in.
//
// Run with:
//   cargo test --test scenario_bulk_solvency -- --nocapture

use solana_sdk::{pubkey::Pubkey, signature::Keypair, signature::Signer};

use crate::common::*;
use openstake_core::constants::MARKET_INIT_FEE;

#[test]
fn thirty_five_bettors_settle_and_claim_stay_solvent() {
    let mut svm = setup();
    let close_ts = default_close_ts(&svm);
    let m = open_market(&mut svm, 100, close_ts);

    // Deterministic spread of sides and sizes - no rand crate needed, and the
    // run is reproducible if something breaks.
    let mut bettors: Vec<(Keypair, Pubkey)> = Vec::with_capacity(35);
    for i in 0..35u64 {
        let side = (i % 2) as u8;
        let deposit = (1 + (i * 3 + 2) % 7) * MARKET_INIT_FEE;

        let (kp, quote_ata) = funded_bettor(&mut svm, deposit + 10 * MARKET_INIT_FEE);
        send(
            &mut svm,
            ix_place_bet(
                &m.market,
                &m.vault,
                &kp.pubkey(),
                &quote_ata,
                side,
                deposit,
                &m.config,
            ),
            &kp,
            &[],
        )
        .unwrap_or_else(|e| panic!("bettor {i} (side {side}, {deposit}) should succeed: {e:?}"));

        bettors.push((kp, quote_ata));
    }

    let pool = token_balance(&svm, &m.vault);

    advance_clock_past(&mut svm, close_ts);
    let authority = authority_keypair();
    let winner = 0u8;
    send(
        &mut svm,
        ix_settle_market(&m.market, &m.vault, &authority.pubkey(), winner),
        &m.creator,
        &[&authority],
    )
    .expect("settle_market should succeed");

    // Everyone with winning shares claims - the creator too, since their
    // seed shares are a real position like anyone else's.
    let mut claimers: Vec<(&Keypair, &Pubkey)> = vec![(&m.creator, &m.creator_quote_ata)];
    for (kp, quote_ata) in &bettors {
        claimers.push((kp, quote_ata));
    }

    let mut paid_out = 0u64;
    let mut winners = 0usize;

    for (kp, quote_ata) in claimers {
        if token_balance(&svm, &outcome_ata(&m.market, &kp.pubkey(), winner)) == 0 {
            continue; // backed the losing side
        }
        let before = token_balance(&svm, quote_ata);
        send(
            &mut svm,
            ix_claim(&m.market, &m.vault, &kp.pubkey(), quote_ata, winner),
            kp,
            &[],
        )
        .unwrap_or_else(|e| panic!("claim for {} should succeed: {e:?}", kp.pubkey()));

        paid_out += token_balance(&svm, quote_ata) - before;
        winners += 1;
    }

    let left_in_vault = token_balance(&svm, &m.vault);

    println!();
    println!("bettors:            36 (35 + creator)");
    println!("winners who claimed: {winners}");
    println!("pool collected:      {pool}");
    println!("paid out:            {paid_out}");
    println!("dust left in vault:  {left_in_vault}");
    println!();

    // The invariant that matters.
    assert!(
        paid_out <= pool,
        "paid out {paid_out} from a pool of {pool} - the vault promised more than it held"
    );

    // Nothing vanished: everything is either paid out or still sitting there.
    assert_eq!(
        paid_out + left_in_vault,
        pool,
        "money went missing somewhere between the pool and the payouts"
    );

    // Floor division always rounds toward the pool, so a few units of dust per
    // claim is expected. What must NOT happen is dust scaling with the amounts
    // moved, which would mean the maths is losing real money.
    assert!(
        left_in_vault < winners as u64 + 1,
        "dust of {left_in_vault} across {winners} claims is more than rounding can explain"
    );
}
