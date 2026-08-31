mod common;
// Demonstration: two markets, two different creators (standing in for two
// different SDK providers), same authority. Proves the "one keypair settles
// everything, for every provider" claim with a real read instead of prose.
//
// Run with:
//   cargo test demo_shared_authority -- --nocapture

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use crate::common::{funded_keypair, ix_initialize_market, market_pda, position_pda, send, setup, setup_with_config, vault_pda, FAR_FUTURE_CLOSE};
use openstake_core::{constants::MARKET_INIT_FEE, amm::state::Market};

#[test]
fn two_unrelated_creators_get_the_exact_same_settlement_authority() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);

    // Stand-in for "Provider A's sports-betting frontend" creating a market
    // on a user's behalf.
    let provider_a_creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let nonce_a = 501u64;
    let (market_a, _) = market_pda(&provider_a_creator.pubkey(), nonce_a);
    let (vault_a, _) = vault_pda(&market_a);
    let (position_a, _) = position_pda(&market_a, &provider_a_creator.pubkey());
    send(
        &mut svm,
        ix_initialize_market(&provider_a_creator.pubkey(), &market_a, &vault_a, &position_a, nonce_a, 2, FAR_FUTURE_CLOSE, &config),
        &provider_a_creator,
        &[],
    )
    .expect("provider A's market should initialize");

    // Stand-in for "Provider B's election-prediction frontend" — a totally
    // different creator, different market, nothing in common with A except
    // that both went through the same Openstake program.
    let provider_b_creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let nonce_b = 777u64;
    let (market_b, _) = market_pda(&provider_b_creator.pubkey(), nonce_b);
    let (vault_b, _) = vault_pda(&market_b);
    let (position_b, _) = position_pda(&market_b, &provider_b_creator.pubkey());
    send(
        &mut svm,
        ix_initialize_market(&provider_b_creator.pubkey(), &market_b, &vault_b, &position_b, nonce_b, 2, FAR_FUTURE_CLOSE, &config),
        &provider_b_creator,
        &[],
    )
    .expect("provider B's market should initialize");

    let market_a_account = svm.get_account(&market_a).unwrap();
    let market_a_state: &Market = from_bytes(&market_a_account.data);
    let market_b_account = svm.get_account(&market_b).unwrap();
    let market_b_state: &Market = from_bytes(&market_b_account.data);

    println!();
    println!("Provider A's creator:  {}", provider_a_creator.pubkey());
    println!("Provider A's market:   {market_a}");
    println!("Provider A's authority: {}", market_a_state.authority);
    println!();
    println!("Provider B's creator:  {}", provider_b_creator.pubkey());
    println!("Provider B's market:   {market_b}");
    println!("Provider B's authority: {}", market_b_state.authority);
    println!();
    println!(
        "same authority despite unrelated creators/markets? {}",
        market_a_state.authority == market_b_state.authority
    );
    println!(
        "compile-time constant they both got: {}",
        openstake_core::constants::AUTHORITY
    );
    println!();

    assert_eq!(
        market_a_state.authority, market_b_state.authority,
        "today, every market from every creator gets the exact same hardcoded authority"
    );
    assert_eq!(market_a_state.authority, openstake_core::constants::AUTHORITY);
}
