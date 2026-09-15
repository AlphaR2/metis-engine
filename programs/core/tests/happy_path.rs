// The one integration test the cleanup keeps: every instruction, in order,
// on the golden path. This is deliberately not exhaustive - it exists to
// prove the six instructions still fit together after a change, not to
// cover edge cases. Edge-case coverage lives back in per-instruction test
// files once the MVP logic (virtual liquidity, the real trade-cap
// mechanism) is implemented and worth pinning down in detail.

mod common;

use common::*;
use solana_sdk::signature::Signer;

#[test]
fn initialize_config_then_market_then_bet_then_settle_then_claim() {
    let mut svm = setup();

    // initialize_config
    let config = setup_with_config(&mut svm);

    // initialize_market - creator seeds a 2-outcome market
    let market = open_market_default(&mut svm, 0);

    // place_bet - a bettor buys outcome 0
    let (bettor, bettor_quote_ata) = funded_bettor(&mut svm, 50_000_000); // $50
    send(
        &mut svm,
        ix_place_bet(
            &market.market,
            &market.vault,
            &bettor.pubkey(),
            &bettor_quote_ata,
            0,
            10_000_000, // $10
            0,          // min_shares_out: accept anything, not testing quote staleness here
            &market.config,
        ),
        &bettor,
        &[],
    )
    .expect("place_bet should succeed");

    let bettor_shares = token_balance(&svm, &outcome_ata(&market.market, &bettor.pubkey(), 0));
    assert!(bettor_shares > 0, "bettor should hold outcome-0 shares after betting");

    // settle_market - advance past kickoff, authority declares outcome 0 the winner
    advance_clock_past(&mut svm, market.bet_close_ts);
    let authority = authority_keypair();
    send(
        &mut svm,
        ix_settle_market(&market.market, &market.vault, &authority.pubkey(), 0),
        &authority,
        &[],
    )
    .expect("settle_market should succeed");

    // claim - the bettor cashes in their winning shares
    let before = token_balance(&svm, &bettor_quote_ata);
    send(
        &mut svm,
        ix_claim(&market.market, &market.vault, &bettor.pubkey(), &bettor_quote_ata, 0),
        &bettor,
        &[],
    )
    .expect("claim should succeed");
    let after = token_balance(&svm, &bettor_quote_ata);

    assert!(after > before, "bettor should receive a payout on claim");
    assert_eq!(
        token_balance(&svm, &outcome_ata(&market.market, &bettor.pubkey(), 0)),
        0,
        "winning shares are burned on claim"
    );

    // update_config - confirm the root-authorized path still works end to end too. Sent "as"
    // one of Config.root's ROOT_SIGNERS, not the settle_market `authority` above - those are
    // two separate authorization checks (Market.authority vs. Config.root).
    let root = root_signer(0);
    send_as(
        &mut svm,
        ix_update_config(&config, &root, 20_000_000, 0, 0, &usdc_mint(), 2),
        root,
    )
    .expect("update_config should succeed");
}
