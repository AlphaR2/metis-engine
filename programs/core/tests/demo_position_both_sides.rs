mod common;
// Demonstration, not a strict assertion suite — prints the actual on-chain
// bytes to settle the "does one Position track both outcomes" question with
// a real read instead of prose.
//
// Run with:
//   cargo test demo_position_both_sides -- --nocapture

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use crate::common::{funded_keypair, ix_initialize_market, ix_place_bet, market_pda, position_pda, send, setup, setup_with_config, vault_pda, FAR_FUTURE_CLOSE};
use openstake_core::{constants::MARKET_INIT_FEE, amm::state::Position};

#[test]
fn one_bettor_holds_both_outcomes_in_a_single_position_account() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let nonce = 999u64;

    let (market, _) = market_pda(&creator.pubkey(), nonce);
    let (vault, _) = vault_pda(&market);
    let (creator_position, _) = position_pda(&market, &creator.pubkey());

    send(
        &mut svm,
        ix_initialize_market(&creator.pubkey(), &market, &vault, &creator_position, nonce, 2, FAR_FUTURE_CLOSE, &config),
        &creator,
        &[],
    )
    .expect("initialize_market should succeed");

    let bettor = funded_keypair(&mut svm, 50 * MARKET_INIT_FEE);
    let (position_yes_pda, _) = position_pda(&market, &bettor.pubkey());
    let (position_no_pda, _) = position_pda(&market, &bettor.pubkey());

    println!();
    println!("bettor address:                 {}", bettor.pubkey());
    println!("position PDA when betting Yes:  {position_yes_pda}");
    println!("position PDA when betting No:   {position_no_pda}");
    println!(
        "same account? {}",
        position_yes_pda == position_no_pda
    );
    println!();

    // Bet #1: $20 on outcome 0 ("Yes"). This CREATES the position account.
    send(
        &mut svm,
        ix_place_bet(&market, &vault, &position_yes_pda, &bettor.pubkey(), 0, 20 * MARKET_INIT_FEE, &config),
        &bettor,
        &[],
    )
    .expect("Yes bet should succeed");

    let after_yes = svm.get_account(&position_yes_pda).unwrap();
    let after_yes_state: &Position = from_bytes(&after_yes.data);
    println!("after betting $20 on Yes only:");
    println!("  position account address: {position_yes_pda}");
    println!("  outcome_shares[0] (Yes):  {}", after_yes_state.outcome_shares[0]);
    println!("  outcome_shares[1] (No):   {}", after_yes_state.outcome_shares[1]);
    println!();

    // Bet #2: same bettor, SAME market, now $10 on outcome 1 ("No"). This
    // hits the exact same PDA — is_data_empty() is now false, so place_bet
    // takes the top-up branch instead of creating a new account.
    send(
        &mut svm,
        ix_place_bet(&market, &vault, &position_no_pda, &bettor.pubkey(), 1, 10 * MARKET_INIT_FEE, &config),
        &bettor,
        &[],
    )
    .expect("No bet should succeed");

    let after_both = svm.get_account(&position_no_pda).unwrap();
    let after_both_state: &Position = from_bytes(&after_both.data);
    println!("after ALSO betting $10 on No, same account:");
    println!("  position account address: {position_no_pda}");
    println!("  outcome_shares[0] (Yes):  {}  <- unchanged from before", after_both_state.outcome_shares[0]);
    println!("  outcome_shares[1] (No):   {}  <- newly added", after_both_state.outcome_shares[1]);
    println!();

    assert_eq!(position_yes_pda, position_no_pda, "one account for both bets, same PDA");
    assert_eq!(
        after_both_state.outcome_shares[0], after_yes_state.outcome_shares[0],
        "the Yes balance must be untouched by the later No bet"
    );
    assert!(after_both_state.outcome_shares[1] > 0, "the No balance must now be non-zero");
}
