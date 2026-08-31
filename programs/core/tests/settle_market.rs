mod common;
// Tier 1 + Tier 2: settle_market — happy path (needs the real authority
// keypair, see authority_keypair() in mod.rs) and the wrong-authority
// rejection (fully testable without it).

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use crate::common::{advance_clock_past, authority_keypair, current_clock_ts, funded_keypair, ix_initialize_market, ix_settle_market, market_pda, position_pda, send, setup, setup_with_config, vault_pda};
use openstake_core::{
    constants::MARKET_INIT_FEE,
    amm::state::{Market, MarketStatus},
};

#[test]
fn settle_market_by_the_real_authority_succeeds() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let nonce = 10u64;

    let (market, _) = market_pda(&creator.pubkey(), nonce);
    let (vault, _) = vault_pda(&market);
    let (position, _) = position_pda(&market, &creator.pubkey());

    let bet_close_ts = current_clock_ts(&svm) + 60;
    let init_ix = ix_initialize_market(&creator.pubkey(), &market, &vault, &position, nonce, 2, bet_close_ts, &config);
    send(&mut svm, init_ix, &creator, &[]).expect("initialize_market should succeed");

    advance_clock_past(&mut svm, bet_close_ts);
    let authority = authority_keypair();
    let settle_ix = ix_settle_market(&market, &vault, &authority.pubkey(), 0);
    send(&mut svm, settle_ix, &creator, &[&authority])
        .expect("settle_market by the real authority should succeed");

    let market_account = svm.get_account(&market).unwrap();
    let market_state: &Market = from_bytes(&market_account.data);
    assert_eq!(market_state.status, MarketStatus::Settled as u8);
    assert_eq!(market_state.winning_outcome, 0);
}

#[test]
fn settle_market_rejects_a_signer_that_isnt_the_authority() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let creator = funded_keypair(&mut svm, 10 * MARKET_INIT_FEE);
    let nonce = 11u64;

    let (market, _) = market_pda(&creator.pubkey(), nonce);
    let (vault, _) = vault_pda(&market);
    let (position, _) = position_pda(&market, &creator.pubkey());

    let init_ix = ix_initialize_market(&creator.pubkey(), &market, &vault, &position, nonce, 2, crate::common::FAR_FUTURE_CLOSE, &config);
    send(&mut svm, init_ix, &creator, &[]).expect("initialize_market should succeed");

    // Anyone who isn't the recorded authority — the creator themselves,
    // in this case, since they're an otherwise-plausible impostor.
    let settle_ix = ix_settle_market(&market, &vault, &creator.pubkey(), 0);
    let result = send(&mut svm, settle_ix, &creator, &[]);

    assert!(
        result.is_err(),
        "settle_market must reject a signer who isn't the market's recorded authority"
    );

    let market_account = svm.get_account(&market).unwrap();
    let market_state: &Market = from_bytes(&market_account.data);
    assert_eq!(
        market_state.status,
        MarketStatus::Open as u8,
        "a rejected settle attempt must leave the market untouched"
    );
}
