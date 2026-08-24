// Tier 1: place_bet happy path — first-time bettor and repeat bettor.

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use super::{
    funded_keypair, ix_initialize_market, ix_place_bet, market_pda, position_pda, send, setup,
    vault_pda,
};
use crate::{
    constants::MARKET_INIT_FEE,
    discriminator::POSITION_DISCRIMINATOR,
    state::{Market, Position},
};

fn create_market(svm: &mut litesvm::LiteSVM, creator: &solana_sdk::signature::Keypair, nonce: u64) -> (
    solana_sdk::pubkey::Pubkey,
    solana_sdk::pubkey::Pubkey,
    solana_sdk::pubkey::Pubkey,
) {
    let (market, _) = market_pda(&creator.pubkey(), nonce);
    let (vault, _) = vault_pda(&market);
    let (creator_position, _) = position_pda(&market, &creator.pubkey());

    let ix = ix_initialize_market(&creator.pubkey(), &market, &vault, &creator_position, nonce, 2);
    send(svm, ix, creator, &[]).expect("initialize_market should succeed");

    (market, vault, creator_position)
}

#[test]
fn place_bet_creates_position_on_first_bet() {
    let mut svm = setup();
    let creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let (market, vault, _) = create_market(&mut svm, &creator, 1);

    let alice = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let (alice_position, _) = position_pda(&market, &alice.pubkey());

    let deposit = 5 * MARKET_INIT_FEE;
    let ix = ix_place_bet(&market, &vault, &alice_position, &alice.pubkey(), 0, deposit);
    send(&mut svm, ix, &alice, &[]).expect("place_bet should succeed for a first-time bettor");

    let position_account = svm.get_account(&alice_position).expect("position exists");
    let position_state: &Position = from_bytes(&position_account.data);

    assert_eq!(position_state.discriminator, POSITION_DISCRIMINATOR);
    assert_eq!(position_state.market, market);
    assert_eq!(position_state.owner, alice.pubkey());
    assert_eq!(position_state.claimed, 0);
    assert!(position_state.outcome_shares[0] > 0, "should hold shares on outcome A");
    assert_eq!(position_state.outcome_shares[1], 0, "shouldn't hold any B shares");

    let market_account = svm.get_account(&market).expect("market exists");
    let market_state: &Market = from_bytes(&market_account.data);
    assert_eq!(market_state.total_collected, MARKET_INIT_FEE + deposit);
}

#[test]
fn place_bet_tops_up_existing_position_on_repeat_bet() {
    let mut svm = setup();
    let creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let (market, vault, _) = create_market(&mut svm, &creator, 2);

    let alice = funded_keypair(&mut svm, 20 * MARKET_INIT_FEE);
    let (alice_position, _) = position_pda(&market, &alice.pubkey());

    let first = 3 * MARKET_INIT_FEE;
    let ix1 = ix_place_bet(&market, &vault, &alice_position, &alice.pubkey(), 0, first);
    send(&mut svm, ix1, &alice, &[]).expect("first bet should succeed");

    let after_first = svm.get_account(&alice_position).unwrap();
    let shares_after_first: &Position = from_bytes(&after_first.data);
    let a_shares_after_first = shares_after_first.outcome_shares[0];

    let second = 2 * MARKET_INIT_FEE;
    let ix2 = ix_place_bet(&market, &vault, &alice_position, &alice.pubkey(), 0, second);
    send(&mut svm, ix2, &alice, &[]).expect("second bet on the same market should top up, not fail");

    let after_second = svm.get_account(&alice_position).unwrap();
    let position_state: &Position = from_bytes(&after_second.data);

    assert!(
        position_state.outcome_shares[0] > a_shares_after_first,
        "shares should have grown after the second bet"
    );

    let market_account = svm.get_account(&market).unwrap();
    let market_state: &Market = from_bytes(&market_account.data);
    assert_eq!(market_state.total_collected, MARKET_INIT_FEE + first + second);
}
