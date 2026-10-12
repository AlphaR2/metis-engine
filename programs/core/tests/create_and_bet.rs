// Creating a market, writing its metadata, and betting on it: one scenario per rule.

mod common;

use common::*;
use metis_core::{
    amm::{
        condition::{DRAW, HOME_WIN, TOTAL_GOALS_OVER},
        curve::{symmetric_seed, virtual_liquidity_per_side},
        quote::{fee_of, quote_bet},
        state::MarketStatus,
    },
    constants::{MIN_SEED, PRIMARY_WINDOW_SECS, VIRTUAL_LIQUIDITY_MULTIPLE},
    error::MetisError,
};
use oracle::instructions::football::CFootballMatchPhase;
use solana_sdk::{pubkey::Pubkey, signature::Signer};

fn world() -> (litesvm::LiteSVM, Pubkey) {
    let mut svm = setup();
    set_clock(&mut svm, 1_000_000);
    let config = setup_with_config(&mut svm);
    (svm, config)
}

// ── Creating ─────────────────────────────────────────────────────────────

#[test]
fn a_market_takes_its_kickoff_from_the_oracle_and_snapshots_the_dials() {
    let mut svm = setup();
    set_clock(&mut svm, 1_000_000);
    let dials = metis_core::config::state::ConfigDials { fee_bps: 25, rake_bps: 150, dispute_secs: 1_800, ..default_dials() };
    let config = setup_with_dials(&mut svm, dials);
    let kickoff = default_close_ts(&svm);
    let m = open_market_with_condition(&mut svm, 0, kickoff, config, TOTAL_GOALS_OVER, 2);

    let market = read_market(&svm, &m.market);
    assert_eq!(market.bet_close_ts, kickoff, "kickoff is the oracle's");
    assert_eq!(market.primary_end, kickoff - PRIMARY_WINDOW_SECS);
    assert_eq!((market.condition_kind, market.condition_param), (TOTAL_GOALS_OVER, 2));
    assert_eq!((market.fee_bps, market.rake_bps, market.dispute_secs), (25, 150, 1_800));
    assert_eq!(market.oracle, m.oracle.to_bytes().into());
    assert_eq!(market.status, MarketStatus::Open as u8);
    assert_eq!(market.total_collected, MIN_SEED);
    assert_eq!((market.fees_accrued, market.payout_pool), (0, 0));

    // The creator holds the seed, and it is all the real shares there are.
    let s = symmetric_seed(MIN_SEED, 2);
    assert_eq!(market.outcome_shares, [s, s]);
    assert_eq!(market.virtual_per_side, virtual_liquidity_per_side(s, VIRTUAL_LIQUIDITY_MULTIPLE).unwrap());
    for side in 0..2u8 {
        let mint = outcome_mint_pda(&m.market, side).0;
        assert_eq!(token_balance(&svm, &outcome_ata(&m.market, &m.creator.pubkey(), side)), s);
        assert_eq!(mint_supply(&svm, &mint), s, "supply equals the recorded shares exactly");
    }
    assert_eq!(token_balance(&svm, &m.vault), MIN_SEED, "the vault holds the seed");
}

#[test]
fn a_market_cannot_be_created_on_a_match_that_has_started_finished_or_been_voided() {
    let (mut svm, config) = world();
    let kickoff = default_close_ts(&svm);

    for (name, edit) in [
        ("under way", (|s: &mut oracle::instructions::football::FootballSettled| s.status = CFootballMatchPhase::FirstHalf) as fn(&mut _)),
        ("finalized", |s| s.finalized = 1),
        ("voided", |s| s.voided = 1),
    ] {
        let (creator, ata) = funded_bettor(&mut svm, 100 * MIN_SEED);
        let oracle = oracle_account(&mut svm, kickoff);
        edit_oracle(&mut svm, &oracle, edit);
        let (market, _) = market_pda(&creator.pubkey(), 0);
        let ix = ix_initialize_market(&creator.pubkey(), &ata, &market, &vault_pda(&market).0, 0, 2, HOME_WIN, 0, &config, &oracle);
        assert_fails_with(send(&mut svm, ix, &creator, &[]), MetisError::OracleNotScheduled);
        assert!(!market_exists(&svm, &market), "{name}: nothing was created");
    }
}

#[test]
fn a_market_needs_the_whole_primary_window_before_kickoff() {
    let (mut svm, config) = world();
    let now = current_clock_ts(&svm);
    for (kickoff, ok) in [(now + PRIMARY_WINDOW_SECS - 1, false), (now + PRIMARY_WINDOW_SECS, true)] {
        let (creator, ata) = funded_bettor(&mut svm, 100 * MIN_SEED);
        let oracle = oracle_account(&mut svm, kickoff);
        let (market, _) = market_pda(&creator.pubkey(), 0);
        let ix = ix_initialize_market(&creator.pubkey(), &ata, &market, &vault_pda(&market).0, 0, 2, HOME_WIN, 0, &config, &oracle);
        let result = send(&mut svm, ix, &creator, &[]);
        if ok {
            result.expect("exactly the window is enough");
        } else {
            assert_fails_with(result, MetisError::OracleNotScheduled);
        }
    }
}

#[test]
fn only_a_real_football_oracle_is_accepted() {
    let (mut svm, config) = world();
    let kickoff = default_close_ts(&svm);
    let (creator, ata) = funded_bettor(&mut svm, 100 * MIN_SEED);
    let (market, _) = market_pda(&creator.pubkey(), 0);
    let make = |svm: &mut litesvm::LiteSVM, owner: Pubkey, len: usize, spoil_discriminator: bool| {
        let address = oracle_account(svm, kickoff);
        let mut account = svm.get_account(&address).unwrap();
        account.owner = owner;
        account.data.resize(len, 0);
        if spoil_discriminator {
            let n = account.data.len();
            account.data[n - 1] ^= 0xff;
        }
        svm.set_account(address, account).unwrap();
        address
    };
    let len = oracle::instructions::football::FootballSettled::LEN;
    for (name, oracle) in [
        ("wrong owner", make(&mut svm, solana_system_interface::program::ID, len, false)),
        ("wrong size", make(&mut svm, oracle_program_id(), len + 1, false)),
        ("wrong discriminator", make(&mut svm, oracle_program_id(), len, true)),
    ] {
        let ix = ix_initialize_market(&creator.pubkey(), &ata, &market, &vault_pda(&market).0, 0, 2, HOME_WIN, 0, &config, &oracle);
        assert_fails_with(send(&mut svm, ix, &creator, &[]), MetisError::InvalidOracleAccount);
        println!("{name}: refused");
    }
}

#[test]
fn an_invalid_condition_is_refused() {
    let (mut svm, config) = world();
    let kickoff = default_close_ts(&svm);
    for (kind, param) in [(9u8, 0u8), (HOME_WIN, 1), (DRAW, 7), (TOTAL_GOALS_OVER, 30)] {
        let (creator, ata) = funded_bettor(&mut svm, 100 * MIN_SEED);
        let oracle = oracle_account(&mut svm, kickoff);
        let (market, _) = market_pda(&creator.pubkey(), 0);
        let ix = ix_initialize_market(&creator.pubkey(), &ata, &market, &vault_pda(&market).0, 0, 2, kind, param, &config, &oracle);
        assert_fails_with(send(&mut svm, ix, &creator, &[]), MetisError::InvalidCondition);
    }
}

// ── Metadata ─────────────────────────────────────────────────────────────

#[test]
fn metadata_is_written_once_by_the_creator_and_reads_back() {
    let (mut svm, config) = world();
    let kickoff = default_close_ts(&svm);
    let m = open_market_with_config(&mut svm, 0, kickoff, config);
    let text = Text::sample();

    send(&mut svm, ix_set_metadata(&m.creator.pubkey(), &m.market, &text), &m.creator, &[]).expect("creator writes it");

    let data = svm.get_account(&metadata_pda(&m.market).0).expect("metadata exists").data;
    let stored: &metis_core::amm::metadata::MarketMetadata = bytemuck::from_bytes(&data);
    assert_eq!(stored.discriminator, metis_core::amm::metadata::MARKET_METADATA_DISCRIMINATOR);
    assert_eq!(stored.market.to_bytes(), m.market.to_bytes());
    assert_eq!(stored.field(0), Some(text.question.as_bytes()));
    assert_eq!(stored.field(1), Some(text.rubric_yes.as_bytes()));
    assert_eq!(stored.field(2), Some(text.rubric_no.as_bytes()));
    assert_eq!(stored.field(3), Some(text.label_yes.as_bytes()));
    assert_eq!(stored.field(6), Some(text.league.as_bytes()));

    // Written once: neither the creator nor anyone else can overwrite what bettors read.
    send(&mut svm, ix_set_metadata(&m.creator.pubkey(), &m.market, &text), &m.creator, &[])
        .expect_err("a second write must fail");
}

#[test]
fn only_the_creator_can_write_metadata_and_only_valid_text_is_kept() {
    let (mut svm, config) = world();
    let kickoff = default_close_ts(&svm);
    let m = open_market_with_config(&mut svm, 0, kickoff, config);

    let outsider = funded_keypair(&mut svm, 1_000_000_000);
    assert_fails_with(
        send(&mut svm, ix_set_metadata(&outsider.pubkey(), &m.market, &Text::sample()), &outsider, &[]),
        MetisError::UnauthorizedAuthority,
    );

    for (name, edit) in [
        ("empty question", (|t: &mut Text| t.question = "") as fn(&mut Text)),
        ("control character", |t| t.rubric_yes = "line one\nline two"),
        ("empty label", |t| t.label_no = ""),
    ] {
        let mut text = Text::sample();
        edit(&mut text);
        assert_fails_with(send(&mut svm, ix_set_metadata(&m.creator.pubkey(), &m.market, &text), &m.creator, &[]), MetisError::InvalidMetadata);
        println!("{name}: refused");
    }
    // A length that points past its buffer is not trusted.
    let mut args = Text::sample().to_args();
    args.lens[0] = 500;
    let mut ix = ix_set_metadata(&m.creator.pubkey(), &m.market, &Text::sample());
    ix.data = [metis_core::amm::state::SET_METADATA.to_vec(), bytemuck::bytes_of(&args).to_vec()].concat();
    assert_fails_with(send(&mut svm, ix, &m.creator, &[]), MetisError::InvalidMetadata);
    assert!(svm.get_account(&metadata_pda(&m.market).0).is_none_or(|a| a.data.is_empty()), "nothing was written");
}

// ── Betting ──────────────────────────────────────────────────────────────

#[test]
fn a_bet_pays_the_fee_into_the_vault_beside_the_pot() {
    let mut svm = setup();
    set_clock(&mut svm, 1_000_000);
    let config = setup_with_dials(&mut svm, metis_core::config::state::ConfigDials { fee_bps: 50, ..default_dials() });
    let kickoff = default_close_ts(&svm);
    let m = open_market_with_config(&mut svm, 0, kickoff, config);
    let (alice, alice_ata) = funded_bettor(&mut svm, 1_000_000_000);

    let before = read_market(&svm, &m.market);
    let deposit = 40_000_000u64;
    // The quote and the program run the same math: the shares must match exactly.
    let quote = quote_bet(&before.outcome_shares, before.virtual_per_side, before.total_collected, 50, 0, 0, deposit).unwrap();
    let shares = bet(&mut svm, &m, &alice, &alice_ata, 0, deposit);
    assert_eq!(shares, quote.shares, "the quote is what the instruction does");

    let after = read_market(&svm, &m.market);
    let fee = fee_of(deposit, 50);
    assert_eq!(fee, 200_000, "0.5% of $40");
    assert_eq!(after.fees_accrued, fee);
    assert_eq!(after.total_collected, before.total_collected + deposit - fee, "only the net enters the pot");
    assert_eq!(after.outcome_shares[0], before.outcome_shares[0] + shares);
    // The vault holds the pot AND the fees - the invariant settlement's solvency check relies on.
    assert_eq!(token_balance(&svm, &m.vault), after.total_collected + after.fees_accrued);
    assert_eq!(token_balance(&svm, &alice_ata), 1_000_000_000 - deposit, "the bettor paid the gross amount");
    assert_eq!(mint_supply(&svm, &outcome_mint_pda(&m.market, 0).0), after.outcome_shares[0], "supply tracks shares");
}

#[test]
fn a_bet_uses_the_fee_the_market_was_created_with_not_todays() {
    let mut svm = setup();
    set_clock(&mut svm, 1_000_000);
    let config = setup_with_dials(&mut svm, metis_core::config::state::ConfigDials { fee_bps: 10, ..default_dials() });
    let kickoff = default_close_ts(&svm);
    let m = open_market_with_config(&mut svm, 0, kickoff, config);
    let root = root_signer(0);
    svm.airdrop(&root, 1_000_000_000).unwrap();
    send_as(&mut svm, ix_update_config(&config, &root, metis_core::config::state::ConfigDials { fee_bps: 50, ..default_dials() }), root).unwrap();

    let (alice, alice_ata) = funded_bettor(&mut svm, 1_000_000_000);
    bet(&mut svm, &m, &alice, &alice_ata, 0, 10_000_000);
    assert_eq!(read_market(&svm, &m.market).fees_accrued, 10_000, "0.10%, not 0.50%");
}

#[test]
fn a_bet_is_refused_when_the_market_is_not_open_for_betting() {
    let (mut svm, config) = world();
    let kickoff = default_close_ts(&svm);
    let m = open_market_with_config(&mut svm, 0, kickoff, config);
    let (alice, alice_ata) = funded_bettor(&mut svm, 1_000_000_000);
    let place = |svm: &mut litesvm::LiteSVM, side: u8, amount: u64, min_out: u64| {
        send(svm, ix_place_bet(&m.market, &m.vault, &alice.pubkey(), &alice_ata, side, amount, min_out, &m.config), &alice, &[])
    };

    assert_fails_with(place(&mut svm, 0, 0, 0), MetisError::PaymentTooSmall);
    assert!(place(&mut svm, 2, 1_000_000, 0).is_err(), "outcome 2 doesn't exist");
    assert_fails_with(place(&mut svm, 0, 1_000_000, u64::MAX), MetisError::SlippageExceeded);

    // At exactly primary_end betting is over.
    let primary_end = read_market(&svm, &m.market).primary_end;
    set_clock(&mut svm, primary_end - 1);
    place(&mut svm, 0, 1_000_000, 0).expect("one second before the end is fine");
    set_clock(&mut svm, primary_end);
    assert_fails_with(place(&mut svm, 0, 1_000_000, 0), MetisError::MarketClosed);

    // Nothing was partly applied by any refusal.
    let after = read_market(&svm, &m.market);
    assert_eq!(token_balance(&svm, &m.vault), after.total_collected + after.fees_accrued);
}

#[test]
fn the_trade_cap_and_price_impact_ceiling_still_stop_an_oversized_bet() {
    let mut svm = setup();
    set_clock(&mut svm, 1_000_000);
    // A 1% cap: of the market's priced depth.
    let config = setup_with_dials(&mut svm, metis_core::config::state::ConfigDials { max_in_bps_of_vault: 100, ..default_dials() });
    let kickoff = default_close_ts(&svm);
    let m = open_market_with_config(&mut svm, 0, kickoff, config);
    let (whale, whale_ata) = funded_bettor(&mut svm, 100_000_000_000);

    let depth = {
        let mk = read_market(&svm, &m.market);
        let priced = metis_core::amm::curve::priced_shares(&mk.outcome_shares, mk.virtual_per_side).unwrap();
        metis_core::amm::curve::cost(&priced) as u64
    };
    let over = depth / 100 + 1_000_000;
    assert_fails_with(
        send(&mut svm, ix_place_bet(&m.market, &m.vault, &whale.pubkey(), &whale_ata, 0, over, 0, &m.config), &whale, &[]),
        MetisError::TradeCapExceeded,
    );
    let under = depth / 100 - 1_000_000;
    send(&mut svm, ix_place_bet(&m.market, &m.vault, &whale.pubkey(), &whale_ata, 0, under, 0, &m.config), &whale, &[]).unwrap();
}
