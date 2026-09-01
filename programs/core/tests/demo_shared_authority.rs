mod common;
// Two markets, two unrelated creators, one settlement authority. Proves the
// "one keypair settles everything, for every provider" claim with a real read
// rather than prose - and shows why replacing it with a quorum matters.
//
// Run with:
//   cargo test --test demo_shared_authority -- --nocapture

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use crate::common::*;
use openstake_core::amm::state::Market;

#[test]
fn unrelated_creators_get_the_same_settlement_authority() {
    let mut svm = setup();
    let config = setup_with_config(&mut svm);
    let close_ts = default_close_ts(&svm);

    // Stand-in for two different frontends creating markets on their users'
    // behalf. Nothing links them except the program itself.
    let a = open_market_with_config(&mut svm, 501, close_ts, config);
    let b = open_market_with_config(&mut svm, 777, close_ts, config);

    let a_account = svm.get_account(&a.market).unwrap();
    let a_state: &Market = from_bytes(&a_account.data);
    let b_account = svm.get_account(&b.market).unwrap();
    let b_state: &Market = from_bytes(&b_account.data);

    println!();
    println!("provider A creator:   {}", a.creator.pubkey());
    println!("provider A market:    {}", a.market);
    println!("provider A authority: {}", a_state.authority);
    println!();
    println!("provider B creator:   {}", b.creator.pubkey());
    println!("provider B market:    {}", b.market);
    println!("provider B authority: {}", b_state.authority);
    println!();
    println!(
        "same authority despite unrelated creators? {}",
        a_state.authority == b_state.authority
    );
    println!(
        "the compile-time constant they both got: {}",
        openstake_core::constants::AUTHORITY
    );
    println!();

    assert_eq!(
        a_state.authority, b_state.authority,
        "today every market from every creator gets the same hardcoded authority"
    );
    assert_eq!(a_state.authority, openstake_core::constants::AUTHORITY);
}
