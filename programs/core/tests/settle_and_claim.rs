// Closing the primary window, settling from the oracle, sweeping fees and claiming: one
// scenario per rule, and the money adds up to the cent at the end.

mod common;

use common::*;
use metis_core::{
    amm::{
        condition::{
            AWAY_GOALS_OVER, AWAY_WIN, AWAY_WIN_BY_OVER, BOTH_TEAMS_SCORE, DRAW, HOME_GOALS_OVER, HOME_WIN,
            HOME_WIN_BY_OVER, TOTAL_GOALS_OVER,
        },
        quote::rake_of,
        state::MarketStatus,
    },
    config::state::ConfigDials,
    constants::{MAX_DISPUTE_SECS, MIN_DISPUTE_SECS},
    error::MetisError,
    settlement::payout::{compute_payout, split_revenue},
};
use solana_sdk::{pubkey::Pubkey, signature::{Keypair, Signer}};

const FEE_BPS: u16 = 50;
const RAKE_BPS: u16 = 100;
const DISPUTE: i64 = MIN_DISPUTE_SECS as i64;

struct Scenario {
    svm: litesvm::LiteSVM,
    m: TestMarket,
    /// (keypair, usdc account, side bet on, amount)
    bettors: Vec<(Keypair, Pubkey, u8, u64)>,
}

/// A market with fees and a rake, four bettors across both sides, ready to be settled.
fn scenario(condition: u8, param: u8) -> Scenario {
    let mut svm = setup();
    set_clock(&mut svm, 1_000_000);
    let dials = ConfigDials { fee_bps: FEE_BPS, rake_bps: RAKE_BPS, dispute_secs: MIN_DISPUTE_SECS, ..default_dials() };
    let config = setup_with_dials(&mut svm, dials);
    let kickoff = default_close_ts(&svm);
    let m = open_market_with_condition(&mut svm, 0, kickoff, config, condition, param);

    let mut bettors = Vec::new();
    for (side, usd) in [(0u8, 40u64), (1, 30), (0, 35), (1, 20)] {
        let (kp, ata) = funded_bettor(&mut svm, 1_000_000_000);
        let amount = usd * 1_000_000;
        bet(&mut svm, &m, &kp, &ata, side, amount);
        bettors.push((kp, ata, side, amount));
    }
    Scenario { svm, m, bettors }
}

impl Scenario {
    /// The match ends with this 90-minute score, final at `at`.
    fn final_score(&mut self, home: u8, away: u8, at: i64) {
        finalize_oracle(&mut self.svm, &self.m.oracle, home, away, at);
    }

    fn settle(&mut self) -> Result<(), litesvm::types::FailedTransactionMetadata> {
        let anyone = funded_operator(&mut self.svm, 1_000_000_000);
        send(&mut self.svm, ix_settle_market(&self.m.market, &self.m.vault, &self.m.oracle, &self.m.config, &anyone.pubkey()), &anyone, &[])
    }

    /// Final at `at`, the dispute window passes, then settle.
    fn settle_with(&mut self, home: u8, away: u8) {
        let at = self.m.bet_close_ts + 7_000;
        self.final_score(home, away, at);
        set_clock(&mut self.svm, at + DISPUTE);
        self.settle().expect("settles");
    }

    fn market(&self) -> metis_core::amm::state::Market {
        read_market(&self.svm, &self.m.market)
    }
}

// ── Closing the primary window ───────────────────────────────────────────

#[test]
fn close_primary_fixes_the_supply_and_creates_both_order_books() {
    let mut s = scenario(HOME_WIN, 0);
    if !load_manifest(&mut s.svm) {
        return;
    }
    let primary_end = s.market().primary_end;
    let caller = funded_operator(&mut s.svm, 5_000_000_000);

    set_clock(&mut s.svm, primary_end - 1);
    assert_fails_with(send(&mut s.svm, ix_close_primary(&caller.pubkey(), &s.m.market, &s.m.config), &caller, &[]), MetisError::PrimaryStillOpen);

    set_clock(&mut s.svm, primary_end);
    send(&mut s.svm, ix_close_primary(&caller.pubkey(), &s.m.market, &s.m.config), &caller, &[]).expect("anyone can close the window");
    assert_eq!(s.market().status, MarketStatus::Closed as u8);

    for side in 0..2u8 {
        // Neither the mint authority nor the freeze authority exists any more.
        let mint = outcome_mint_pda(&s.m.market, side).0;
        let data = s.svm.get_account(&mint).unwrap().data;
        assert_eq!(u32::from_le_bytes(data[0..4].try_into().unwrap()), 0, "mint authority revoked");
        assert_eq!(u32::from_le_bytes(data[46..50].try_into().unwrap()), 0, "freeze authority revoked");

        // The book exists at the derived address, owned by Manifest, for exactly this pair.
        let book = s.svm.get_account(&book_pda(&s.m.market, side)).expect("book exists");
        assert_eq!(book.owner, manifest_program_id());
        assert_eq!(&book.data[16..48], mint.as_ref(), "base mint is the outcome token");
        assert_eq!(&book.data[48..80], usdc_mint().as_ref(), "quote mint is USDC");
    }

    // Once closed it stays closed: no second close, and no more bets.
    assert_fails_with(send(&mut s.svm, ix_close_primary(&caller.pubkey(), &s.m.market, &s.m.config), &caller, &[]), MetisError::InvalidStatusTransition);
    let (late, late_ata) = funded_bettor(&mut s.svm, 100_000_000);
    assert_fails_with(
        send(&mut s.svm, ix_place_bet(&s.m.market, &s.m.vault, &late.pubkey(), &late_ata, 0, 1_000_000, 0, &s.m.config), &late, &[]),
        MetisError::MarketNotOpen,
    );
}

#[test]
fn close_primary_refuses_books_and_mints_that_are_not_the_markets_own() {
    let mut s = scenario(HOME_WIN, 0);
    if !load_manifest(&mut s.svm) {
        return;
    }
    let primary_end = s.market().primary_end;
    set_clock(&mut s.svm, primary_end);
    let caller = funded_operator(&mut s.svm, 5_000_000_000);

    // Books swapped: book 1's address where book 0 belongs.
    let mut ix = ix_close_primary(&caller.pubkey(), &s.m.market, &s.m.config);
    ix.accounts.swap(5, 6);
    assert_fails_with(send(&mut s.svm, ix, &caller, &[]), MetisError::InvalidPda);

    // A mint that isn't this market's.
    let mut ix = ix_close_primary(&caller.pubkey(), &s.m.market, &s.m.config);
    ix.accounts[3].pubkey = usdc_mint();
    assert_fails_with(send(&mut s.svm, ix, &caller, &[]), MetisError::InvalidPda);
    assert_eq!(s.market().status, MarketStatus::Open as u8, "nothing was applied");
}

// ── Settling ─────────────────────────────────────────────────────────────

#[test]
fn settlement_waits_for_a_final_oracle_result_and_this_markets_dispute_window() {
    let mut s = scenario(HOME_WIN, 0);
    let at = s.m.bet_close_ts + 7_000;

    // The oracle hasn't produced a result: nothing to settle on.
    set_clock(&mut s.svm, at);
    assert_fails_with(s.settle(), MetisError::OracleNotFinal);

    // Final, but the window in which two roots could still override it is open.
    s.final_score(2, 0, at);
    assert_fails_with(s.settle(), MetisError::DisputeWindowOpen);
    set_clock(&mut s.svm, at + DISPUTE - 1);
    assert_fails_with(s.settle(), MetisError::DisputeWindowOpen);

    // The window closes: anyone can settle.
    set_clock(&mut s.svm, at + DISPUTE);
    s.settle().expect("settles");
    let m = s.market();
    assert_eq!(m.status, MarketStatus::Settled as u8);
    assert_eq!(m.resolved_at, at + DISPUTE);

    // And only once.
    assert_fails_with(s.settle(), MetisError::InvalidStatusTransition);
}

#[test]
fn a_voided_oracle_cannot_settle_a_market() {
    let mut s = scenario(HOME_WIN, 0);
    let at = s.m.bet_close_ts + 7_000;
    void_oracle(&mut s.svm, &s.m.oracle, at);
    set_clock(&mut s.svm, at + MAX_DISPUTE_SECS as i64);
    assert_fails_with(s.settle(), MetisError::OracleVoided);
}

#[test]
fn settlement_only_accepts_the_oracle_the_market_was_created_against() {
    let mut s = scenario(HOME_WIN, 0);
    let at = s.m.bet_close_ts + 7_000;
    s.final_score(2, 0, at);
    set_clock(&mut s.svm, at + DISPUTE);

    // A different, perfectly real, finalised oracle that says the opposite.
    let impostor = oracle_account(&mut s.svm, s.m.bet_close_ts);
    finalize_oracle(&mut s.svm, &impostor, 0, 5, at);
    let anyone = funded_operator(&mut s.svm, 1_000_000_000);
    assert_fails_with(
        send(&mut s.svm, ix_settle_market(&s.m.market, &s.m.vault, &impostor, &s.m.config, &anyone.pubkey()), &anyone, &[]),
        MetisError::OracleMismatch,
    );

    // An account that isn't an oracle at all.
    assert_fails_with(
        send(&mut s.svm, ix_settle_market(&s.m.market, &s.m.vault, &s.m.vault, &s.m.config, &anyone.pubkey()), &anyone, &[]),
        MetisError::InvalidOracleAccount,
    );
    assert_eq!(s.market().status, MarketStatus::Open as u8);
}

#[test]
fn each_condition_picks_the_right_winner_from_the_ninety_minute_score() {
    // (condition, param, score, expected winning outcome: 0 = YES, 1 = NO)
    let cases = [
        (HOME_WIN, 0, (2, 1), 0u8),
        (HOME_WIN, 0, (1, 1), 1),
        (HOME_WIN, 0, (0, 2), 1),
        (AWAY_WIN, 0, (0, 2), 0),
        (AWAY_WIN, 0, (3, 3), 1),
        (DRAW, 0, (1, 1), 0),
        (DRAW, 0, (2, 1), 1),
        (TOTAL_GOALS_OVER, 2, (2, 1), 0),
        (TOTAL_GOALS_OVER, 2, (1, 1), 1),
        (BOTH_TEAMS_SCORE, 0, (1, 1), 0),
        (BOTH_TEAMS_SCORE, 0, (3, 0), 1),
        (HOME_GOALS_OVER, 3, (4, 1), 0),
        (HOME_GOALS_OVER, 3, (3, 3), 1),
        (AWAY_GOALS_OVER, 1, (0, 2), 0),
        (HOME_WIN_BY_OVER, 3, (4, 0), 0),
        (HOME_WIN_BY_OVER, 3, (3, 0), 1),
        (AWAY_WIN_BY_OVER, 2, (0, 3), 0),
        (AWAY_WIN_BY_OVER, 2, (1, 3), 1),
    ];
    for (kind, param, (h, a), expected) in cases {
        let mut s = scenario(kind, param);
        s.settle_with(h, a);
        assert_eq!(s.market().winning_outcome, expected, "condition {kind}/{param} on {h}-{a}");
    }
}

#[test]
fn a_market_is_judged_on_the_ninety_minute_score_never_the_final_one() {
    // 1-1 after 90 minutes, 3-1 after extra time. "Home win" was about the 90 minutes: NO wins.
    let mut s = scenario(HOME_WIN, 0);
    let at = s.m.bet_close_ts + 7_000;
    finalize_oracle(&mut s.svm, &s.m.oracle, 1, 1, at);
    edit_oracle(&mut s.svm, &s.m.oracle, |o| o.score = oracle::instructions::football::FootballSettled::pack_score(3, 1));
    set_clock(&mut s.svm, at + DISPUTE);
    s.settle().unwrap();
    assert_eq!(s.market().winning_outcome, 1, "a draw at 90 minutes is NO, whatever happened after");
}

#[test]
fn settlement_records_the_rake_the_fees_and_what_winners_share() {
    let mut s = scenario(HOME_WIN, 0);
    let before = s.market();
    s.settle_with(2, 0);
    let m = s.market();

    let rake = rake_of(before.total_collected, RAKE_BPS);
    assert_eq!(m.payout_pool, before.total_collected - rake, "winners share the pot minus the rake");

    // The rake and the fees are one pot of revenue, shared four ways. This scenario has no
    // platform and a creator-seeded market, so the platform's share stays with the protocol and the
    // backers' share goes to the creator.
    let shares = [m.fee_share_protocol_bps, m.fee_share_backers_bps, m.fee_share_creator_bps, m.fee_share_platform_bps];
    let split = split_revenue(rake + before.fees_accrued, shares, true, false);
    assert_eq!(m.protocol_owed, split.protocol, "the protocol: its share, the unnamed platform's and the rounding dust");
    assert_eq!(m.creator_owed, split.creator, "the seed holder: the creator's share and the backers'");
    assert_eq!((m.platform_owed, m.backers_owed), (0, 0));
    assert!(m.creator_owed > before.fees_accrued * m.fee_share_creator_bps as u64 / 10_000, "the rake is shared too, not just the fees");

    // Nothing is paid out inside settlement, and every cent in the vault is accounted for.
    assert_eq!(
        token_balance(&s.svm, &s.m.vault),
        m.payout_pool + m.protocol_owed + m.creator_owed + m.platform_owed + m.backers_owed
    );
    assert_eq!(shares.iter().map(|b| *b as u64).sum::<u64>(), 10_000);
}

#[test]
fn a_platform_earns_its_share_of_the_rake_and_the_fees_and_sweeps_it_to_itself() {
    let mut svm = setup();
    set_clock(&mut svm, 1_000_000);
    let dials = ConfigDials { fee_bps: FEE_BPS, rake_bps: RAKE_BPS, dispute_secs: MIN_DISPUTE_SECS, ..default_dials() };
    let config = setup_with_dials(&mut svm, dials);
    let kickoff = default_close_ts(&svm);
    let (platform, platform_ata) = funded_bettor(&mut svm, 1);
    let m = open_market_on_platform(&mut svm, 0, kickoff, config, &platform.pubkey());
    for (side, usd) in [(0u8, 40u64), (1, 30)] {
        let (kp, ata) = funded_bettor(&mut svm, 1_000_000_000);
        bet(&mut svm, &m, &kp, &ata, side, usd * 1_000_000);
    }
    let before = read_market(&svm, &m.market);
    assert_eq!(before.platform.to_bytes(), platform.pubkey().to_bytes(), "the platform is recorded on the market");

    let at = kickoff + 7_000;
    finalize_oracle(&mut svm, &m.oracle, 2, 0, at);
    set_clock(&mut svm, at + DISPUTE);
    let anyone = funded_operator(&mut svm, 1_000_000_000);
    send(&mut svm, ix_settle_market(&m.market, &m.vault, &m.oracle, &m.config, &anyone.pubkey()), &anyone, &[]).expect("settles");

    let settled = read_market(&svm, &m.market);
    let rake = rake_of(before.total_collected, RAKE_BPS);
    let shares = [settled.fee_share_protocol_bps, settled.fee_share_backers_bps, settled.fee_share_creator_bps, settled.fee_share_platform_bps];
    let split = split_revenue(rake + before.fees_accrued, shares, true, true);
    assert!(split.platform > 0, "the platform's cut is real money in this scenario");
    assert_eq!(settled.platform_owed, split.platform);
    assert_eq!(settled.protocol_owed + settled.creator_owed + settled.platform_owed, rake + before.fees_accrued);

    // Anyone may trigger the platform's sweep, but it can only land in the platform's own account.
    let creator_ata = m.creator_quote_ata;
    assert_fails_with(
        send(&mut svm, ix_sweep_fees(&m.market, &m.vault, &creator_ata, &m.config, 2, &anyone.pubkey()), &anyone, &[]),
        MetisError::WrongAccountReference,
    );
    let held = token_balance(&svm, &platform_ata);
    send(&mut svm, ix_sweep_fees(&m.market, &m.vault, &platform_ata, &m.config, 2, &anyone.pubkey()), &anyone, &[]).expect("platform sweep");
    assert_eq!(token_balance(&svm, &platform_ata) - held, split.platform);
    assert_fails_with(
        send(&mut svm, ix_sweep_fees(&m.market, &m.vault, &platform_ata, &m.config, 2, &anyone.pubkey()), &anyone, &[]),
        MetisError::NothingToSweep,
    );
}

// ── Sweeping fees ────────────────────────────────────────────────────────

#[test]
fn the_protocol_and_the_creator_each_sweep_their_own_share_exactly_once() {
    let mut s = scenario(HOME_WIN, 0);
    let t_ata = treasury_ata(&mut s.svm);
    let creator_ata = s.m.creator_quote_ata;
    let anyone = funded_operator(&mut s.svm, 1_000_000_000);
    let sweep = |s: &mut Scenario, ata: &Pubkey, which: u8| {
        send(&mut s.svm, ix_sweep_fees(&s.m.market, &s.m.vault, ata, &s.m.config, which, &anyone.pubkey()), &anyone, &[])
    };

    assert_fails_with(sweep(&mut s, &t_ata, 0), MetisError::MarketNotSettled);
    s.settle_with(2, 0);
    let m = s.market();

    // Each share must go to its rightful owner's account - not to whoever asks.
    assert_fails_with(sweep(&mut s, &creator_ata, 0), MetisError::WrongAccountReference);
    assert_fails_with(sweep(&mut s, &t_ata, 1), MetisError::WrongAccountReference);

    sweep(&mut s, &t_ata, 0).expect("protocol sweep");
    assert_eq!(token_balance(&s.svm, &t_ata), m.protocol_owed);
    let creator_before = token_balance(&s.svm, &s.m.creator_quote_ata);
    sweep(&mut s, &creator_ata, 1).expect("creator sweep");
    assert_eq!(token_balance(&s.svm, &s.m.creator_quote_ata) - creator_before, m.creator_owed);

    // Once each: a repeat finds nothing owed.
    assert_fails_with(sweep(&mut s, &t_ata, 0), MetisError::NothingToSweep);
    assert_fails_with(sweep(&mut s, &creator_ata, 1), MetisError::NothingToSweep);
    assert_eq!((s.market().protocol_owed, s.market().creator_owed), (0, 0));
}

#[test]
fn one_party_failing_to_collect_cannot_block_the_other_or_any_claim() {
    let mut s = scenario(HOME_WIN, 0);
    s.settle_with(2, 0);

    // The creator's USDC account is closed: their own sweep can't work...
    s.svm.set_account(s.m.creator_quote_ata, solana_sdk::account::Account::default()).unwrap();
    let t_ata = treasury_ata(&mut s.svm);
    let anyone = funded_operator(&mut s.svm, 1_000_000_000);
    assert!(send(&mut s.svm, ix_sweep_fees(&s.m.market, &s.m.vault, &s.m.creator_quote_ata, &s.m.config, 1, &anyone.pubkey()), &anyone, &[]).is_err());

    // ...but the protocol's still does, and so do the winners' claims.
    send(&mut s.svm, ix_sweep_fees(&s.m.market, &s.m.vault, &t_ata, &s.m.config, 0, &anyone.pubkey()), &anyone, &[]).expect("protocol unaffected");
    let (winner, winner_ata, _, _) = &s.bettors[0];
    let before = token_balance(&s.svm, winner_ata);
    send(&mut s.svm, ix_claim(&s.m.market, &s.m.vault, &winner.pubkey(), winner_ata, 0), winner, &[]).expect("claims unaffected");
    assert!(token_balance(&s.svm, winner_ata) > before);
}

// ── Claiming, and the money adding up ────────────────────────────────────

#[test]
fn winners_split_the_pool_after_the_rake_and_every_cent_is_accounted_for() {
    let mut s = scenario(HOME_WIN, 0);
    s.settle_with(2, 0); // home win: outcome 0 (YES) wins
    let m = s.market();
    let winning_shares = m.outcome_shares[0];
    let t_ata = treasury_ata(&mut s.svm);

    let mut paid_to_bettors = 0u64;
    for (kp, ata, side, _amount) in &s.bettors {
        let shares = token_balance(&s.svm, &outcome_ata(&s.m.market, &kp.pubkey(), *side));
        let before = token_balance(&s.svm, ata);
        if *side == 0 {
            send(&mut s.svm, ix_claim(&s.m.market, &s.m.vault, &kp.pubkey(), ata, 0), kp, &[]).expect("winner claims");
            let got = token_balance(&s.svm, ata) - before;
            assert_eq!(got, compute_payout(shares, m.payout_pool, winning_shares), "exactly its slice of the pool");
            paid_to_bettors += got;
            // A second claim finds nothing left to cash in.
            assert_fails_with(send(&mut s.svm, ix_claim(&s.m.market, &s.m.vault, &kp.pubkey(), ata, 0), kp, &[]), MetisError::AlreadyClaimed);
        } else {
            // A loser holds no winning shares, so there is nothing for them to claim.
            assert!(send(&mut s.svm, ix_claim(&s.m.market, &s.m.vault, &kp.pubkey(), ata, 0), kp, &[]).is_err());
            assert_eq!(token_balance(&s.svm, ata), before);
        }
    }

    // The seed holder (the creator) holds winning shares too, and claims like anyone else.
    let creator = &s.m.creator;
    let cbefore = token_balance(&s.svm, &s.m.creator_quote_ata);
    send(&mut s.svm, ix_claim(&s.m.market, &s.m.vault, &creator.pubkey(), &s.m.creator_quote_ata, 0), creator, &[]).unwrap();
    paid_to_bettors += token_balance(&s.svm, &s.m.creator_quote_ata) - cbefore;

    let anyone = funded_operator(&mut s.svm, 1_000_000_000);
    send(&mut s.svm, ix_sweep_fees(&s.m.market, &s.m.vault, &t_ata, &s.m.config, 0, &anyone.pubkey()), &anyone, &[]).unwrap();
    send(&mut s.svm, ix_sweep_fees(&s.m.market, &s.m.vault, &s.m.creator_quote_ata, &s.m.config, 1, &anyone.pubkey()), &anyone, &[]).unwrap();

    // Everything that went in came out, apart from rounding dust left in the vault.
    let total_in = m.total_collected + m.fees_accrued;
    let dust = token_balance(&s.svm, &s.m.vault);
    let paid_out = paid_to_bettors + m.protocol_owed + m.creator_owed + m.platform_owed + m.backers_owed;
    assert_eq!(paid_out + dust, total_in, "the vault balance explains itself exactly");
    assert!(dust <= 5, "rounding dust is a few atoms, got {dust}");
}

#[test]
fn nobody_can_claim_before_settlement() {
    let mut s = scenario(HOME_WIN, 0);
    let (kp, ata, _, _) = &s.bettors[0];
    assert_fails_with(send(&mut s.svm, ix_claim(&s.m.market, &s.m.vault, &kp.pubkey(), ata, 0), kp, &[]), MetisError::MarketNotSettled);
}
