mod common;
// Recording the result. Who may do it, when, and what has to be true about
// the vault before it's allowed.
//
// To see the printed output:
//   cargo test -p openstake-core --test settle_market -- --nocapture --test-threads=1

use bytemuck::from_bytes;
use solana_sdk::signature::Signer;

use crate::common::*;
use openstake_core::{
    amm::state::{Market, MarketStatus},
    constants::MARKET_INIT_FEE,
};

fn usd(raw: u64) -> String {
    format!("${:.2}", raw as f64 / 1e6)
}

// The happy path. Settling writes down which outcome won and flips the market
// to Settled - it runs no pricing at all, the curve is finished by this point.
#[test]
fn the_recorded_authority_can_settle() {
    let mut svm = setup();
    let close_ts = default_close_ts(&svm);
    let m = open_market(&mut svm, 20, close_ts);

    advance_clock_past(&mut svm, close_ts);
    let authority = authority_keypair();

    send(
        &mut svm,
        ix_settle_market(&m.market, &m.vault, &authority.pubkey(), 1),
        &m.creator,
        &[&authority],
    )
    .expect("settle_market should succeed");

    let market_account = svm.get_account(&m.market).unwrap();
    let market_state: &Market = from_bytes(&market_account.data);

    println!();
    println!("settled by {}", authority.pubkey());
    println!("  status:          {} (Settled)", market_state.status);
    println!("  winning outcome: {}", market_state.winning_outcome);
    println!("  pool to split:   {}", usd(market_state.total_collected));
    println!(
        "  winning shares:  {:.2}  -> {:.4} per share",
        market_state.outcome_shares[1] as f64 / 1e6,
        market_state.total_collected as f64 / market_state.outcome_shares[1] as f64
    );
    println!();

    assert_eq!(market_state.status, MarketStatus::Settled as u8);
    assert_eq!(market_state.winning_outcome, 1);
}

// Settlement is the one thing in this program that isn't permissionless -
// somebody has to state what actually happened. Only the key recorded on the
// market may do it.
#[test]
fn anyone_else_signing_is_rejected() {
    let mut svm = setup();
    let close_ts = default_close_ts(&svm);
    let m = open_market(&mut svm, 21, close_ts);

    advance_clock_past(&mut svm, close_ts);
    let impostor = funded_keypair(&mut svm, 10 * solana_sdk::native_token::LAMPORTS_PER_SOL);

    let result = send(
        &mut svm,
        ix_settle_market(&m.market, &m.vault, &impostor.pubkey(), 0),
        &impostor,
        &[],
    );

    println!();
    println!("real authority: {}", authority_keypair().pubkey());
    println!("impostor:       {}", impostor.pubkey());
    println!(
        "settle -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(
        result.is_err(),
        "only the market's recorded authority may settle"
    );
}

// There's no result to report until the event has actually started, so
// settling early is meaningless at best and manipulation at worst.
#[test]
fn settling_before_kickoff_is_rejected() {
    let mut svm = setup();
    let m = open_market_default(&mut svm, 22);

    let authority = authority_keypair();
    let result = send(
        &mut svm,
        ix_settle_market(&m.market, &m.vault, &authority.pubkey(), 0),
        &m.creator,
        &[&authority],
    );

    let market_account = svm.get_account(&m.market).unwrap();
    let market_state: &Market = from_bytes(&market_account.data);

    println!();
    println!("clock now: {}", current_clock_ts(&svm));
    println!("kickoff:   {}", market_state.bet_close_ts);
    println!(
        "settle -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!("market status still {} (Open)", market_state.status);
    println!();

    assert!(result.is_err(), "settling before kickoff must be rejected");
    assert_eq!(
        market_state.status,
        MarketStatus::Open as u8,
        "a rejected settle must leave the market untouched"
    );
}

// A settled market is final. Allowing a second settle would let the authority
// change the winner after people had started claiming against the first one.
#[test]
fn settling_twice_is_rejected() {
    let mut svm = setup();
    let close_ts = default_close_ts(&svm);
    let m = open_market(&mut svm, 23, close_ts);

    advance_clock_past(&mut svm, close_ts);
    let authority = authority_keypair();

    send(
        &mut svm,
        ix_settle_market(&m.market, &m.vault, &authority.pubkey(), 0),
        &m.creator,
        &[&authority],
    )
    .expect("first settle");

    let result = send(
        &mut svm,
        ix_settle_market(&m.market, &m.vault, &authority.pubkey(), 1),
        &m.creator,
        &[&authority],
    );

    let market_account = svm.get_account(&m.market).unwrap();
    let market_state: &Market = from_bytes(&market_account.data);

    println!();
    println!("first settle:  outcome 0 -> accepted");
    println!(
        "second settle: outcome 1 -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!("winner is still {}", market_state.winning_outcome);
    println!();

    assert!(
        result.is_err(),
        "a settled market must not be settled again"
    );
    assert_eq!(
        market_state.winning_outcome, 0,
        "the original winner must stand"
    );
}

// The winner has to be an outcome this market actually has. An index past the
// end would leave claim reading a share count that was never traded.
#[test]
fn an_out_of_range_winner_is_rejected() {
    let mut svm = setup();
    let close_ts = default_close_ts(&svm);
    let m = open_market(&mut svm, 24, close_ts);

    advance_clock_past(&mut svm, close_ts);
    let authority = authority_keypair();

    let result = send(
        &mut svm,
        ix_settle_market(&m.market, &m.vault, &authority.pubkey(), 2), // only 0 and 1 exist
        &m.creator,
        &[&authority],
    );

    println!();
    println!(
        "market has outcomes 0 and 1; settling on 2 -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(result.is_err(), "winning_outcome must be a real outcome");
}

// Anyone can send tokens to any account. If the solvency check demanded an
// exact balance, one stray dollar would brick the market permanently - money
// only leaves through claim, and claim needs the market Settled first. So the
// check is a floor, not an equality.
#[test]
fn a_vault_surplus_does_not_block_settlement() {
    let mut svm = setup();
    let close_ts = default_close_ts(&svm);
    let m = open_market(&mut svm, 25, close_ts);

    let expected = token_balance(&svm, &m.vault);
    let inflated = expected + 12_345;
    let stray = fund_usdc(&mut svm, &m.vault, inflated);
    assert_eq!(stray, m.vault, "vault is its own token account, not an ATA");

    println!();
    println!("market collected: {}", usd(expected));
    println!(
        "vault now holds:  {}  (someone sent {} unprompted)",
        usd(inflated),
        usd(12_345)
    );

    advance_clock_past(&mut svm, close_ts);
    let authority = authority_keypair();

    send(
        &mut svm,
        ix_settle_market(&m.market, &m.vault, &authority.pubkey(), 0),
        &m.creator,
        &[&authority],
    )
    .expect("a vault surplus must not block settlement");

    println!("settle -> accepted (a surplus must never brick a market)");
    println!();
}

// The mirror of the above. If the vault somehow holds less than was paid in,
// the payouts can't all be honoured, so settling has to be refused rather than
// letting the first few claimers drain what's there.
#[test]
fn a_short_vault_blocks_settlement() {
    let mut svm = setup();
    let close_ts = default_close_ts(&svm);
    let m = open_market(&mut svm, 26, close_ts);

    let (bettor, quote_ata) = funded_bettor(&mut svm, 50 * MARKET_INIT_FEE);
    send(
        &mut svm,
        ix_place_bet(
            &m.market,
            &m.vault,
            &bettor.pubkey(),
            &quote_ata,
            0,
            10 * MARKET_INIT_FEE,
            &m.config,
        ),
        &bettor,
        &[],
    )
    .expect("bet");

    let owed = {
        let a = svm.get_account(&m.market).unwrap();
        let s: &Market = from_bytes(&a.data);
        s.total_collected
    };

    // Drain the vault behind the program's back.
    fund_usdc(&mut svm, &m.vault, 1);

    println!();
    println!("market says it collected: {}", usd(owed));
    println!(
        "vault actually holds:     {}",
        usd(token_balance(&svm, &m.vault))
    );

    advance_clock_past(&mut svm, close_ts);
    let authority = authority_keypair();

    let result = send(
        &mut svm,
        ix_settle_market(&m.market, &m.vault, &authority.pubkey(), 0),
        &m.creator,
        &[&authority],
    );

    println!(
        "settle -> {}",
        if result.is_err() {
            "rejected"
        } else {
            "ACCEPTED (bug)"
        }
    );
    println!();

    assert!(result.is_err(), "an insolvent vault must block settlement");
}
