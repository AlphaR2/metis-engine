mod common;
use bytemuck::from_bytes;

use crate::common::{funded_keypair, ix_initialize_market, market_pda, position_pda, send, setup, setup_with_config, vault_pda, FAR_FUTURE_CLOSE};
use openstake_core::{
    constants::{MARKET_INIT_FEE, OUTCOME_UNRESOLVED},
    amm::state::{Market, MarketStatus, Position, MARKET_DISCRIMINATOR, POSITION_DISCRIMINATOR}, settlement::state::{Vault, VAULT_DISCRIMINATOR},
};
use solana_sdk::signature::Signer;

#[test]
fn initialize_market_creates_market_vault_and_creator_position() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);

    let nonce = 1u64;
    let num_outcomes = 2u8;

    let (market, _market_bump) = market_pda(&creator.pubkey(), nonce);
    let (vault, _vault_bump) = vault_pda(&market);
    let (position, _position_bump) = position_pda(&market, &creator.pubkey());

    let ix = ix_initialize_market(
        &creator.pubkey(),
        &market,
        &vault,
        &position,
        nonce,
        num_outcomes,
        FAR_FUTURE_CLOSE,
        &config,
    );
    send(&mut svm, ix, &creator, &[]).expect("initialize_market should succeed");

    // Market: correct discriminator, balanced seed, Open, no winner yet.
    let market_account = svm.get_account(&market).expect("market account exists");
    let market_state: &Market = from_bytes(&market_account.data);

    assert_eq!(market_state.discriminator, MARKET_DISCRIMINATOR);
    assert_eq!(market_state.status, MarketStatus::Open as u8);
    assert_eq!(market_state.winning_outcome, OUTCOME_UNRESOLVED);
    assert_eq!(market_state.num_outcomes, num_outcomes);
    assert_eq!(market_state.total_collected, MARKET_INIT_FEE);
    assert_eq!(market_state.outcome_shares[0], market_state.outcome_shares[1]);
    assert!(market_state.outcome_shares[0] > 0, "seed shares should be non-zero");
    assert_eq!(market_state.vault, vault);
    assert_eq!(market_state.creator, creator.pubkey());
    assert_eq!(market_state.bet_close_ts, FAR_FUTURE_CLOSE);

    // Vault: correct discriminator, points back at the market, funded.
    let vault_account = svm.get_account(&vault).expect("vault account exists");
    let vault_state: &Vault = from_bytes(&vault_account.data);
    assert_eq!(vault_state.discriminator, VAULT_DISCRIMINATOR);
    assert_eq!(vault_state.market, market);
    assert!(
        vault_account.lamports >= MARKET_INIT_FEE,
        "vault should hold at least the creator's fee"
    );

    // Position: creator holds the same seed shares as the market opened with.
    let position_account = svm.get_account(&position).expect("position account exists");
    let position_state: &Position = from_bytes(&position_account.data);
    assert_eq!(position_state.discriminator, POSITION_DISCRIMINATOR);
    assert_eq!(position_state.market, market);
    assert_eq!(position_state.owner, creator.pubkey());
    assert_eq!(position_state.claimed, 0);
    assert_eq!(position_state.outcome_shares, market_state.outcome_shares);
}
