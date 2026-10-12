// Cancelling a market and refunding everyone, closing it for good, and what pausing does and
// does not stop. One scenario per rule.

mod common;

use common::*;
use metis_core::{
    amm::{condition::HOME_WIN, state::MarketStatus},
    config::state::ConfigDials,
    constants::{CANCEL_AFTER_SECS, CLAIM_WINDOW_SECS, MIN_DISPUTE_SECS},
    error::MetisError,
    settlement::payout::compute_payout,
};
use solana_sdk::{
    account::Account,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

struct World {
    svm: litesvm::LiteSVM,
    m: TestMarket,
    bettors: Vec<(Keypair, Pubkey, u8)>,
}

/// A market with fees, a rake and four bettors across both sides.
fn world() -> World {
    let mut svm = setup();
    set_clock(&mut svm, 1_000_000);
    let dials = ConfigDials { fee_bps: 50, rake_bps: 100, dispute_secs: MIN_DISPUTE_SECS, ..default_dials() };
    let config = setup_with_dials(&mut svm, dials);
    let kickoff = default_close_ts(&svm);
    let m = open_market_with_condition(&mut svm, 0, kickoff, config, HOME_WIN, 0);
    let mut bettors = Vec::new();
    for (side, usd) in [(0u8, 40u64), (1, 30), (0, 35), (1, 20)] {
        let (kp, ata) = funded_bettor(&mut svm, 1_000_000_000);
        bet(&mut svm, &m, &kp, &ata, side, usd * 1_000_000);
        bettors.push((kp, ata, side));
    }
    World { svm, m, bettors }
}

impl World {
    fn market(&self) -> metis_core::amm::state::Market {
        read_market(&self.svm, &self.m.market)
    }

    fn cancel(&mut self, roots: Option<(Pubkey, Pubkey)>) -> Result<(), litesvm::types::FailedTransactionMetadata> {
        match roots {
            Some((first, second)) => {
                let ix = ix_cancel_market(&self.m.market, &self.m.oracle, &self.m.config, &first, Some((first, second)));
                send_as(&mut self.svm, ix, first)
            }
            None => {
                let op = funded_operator(&mut self.svm, 1_000_000_000);
                self.cancel_by(&op)
            }
        }
    }

    /// A one-signature cancel by `who`, operator or not.
    fn cancel_by(&mut self, who: &Keypair) -> Result<(), litesvm::types::FailedTransactionMetadata> {
        let ix = ix_cancel_market(&self.m.market, &self.m.oracle, &self.m.config, &who.pubkey(), None);
        send(&mut self.svm, ix, who, &[])
    }

    fn two_roots() -> Option<(Pubkey, Pubkey)> {
        Some((root_signer(0), root_signer(1)))
    }

    fn settle(&mut self, home: u8, away: u8) {
        let at = self.m.bet_close_ts + 7_000;
        finalize_oracle(&mut self.svm, &self.m.oracle, home, away, at);
        set_clock(&mut self.svm, at + MIN_DISPUTE_SECS as i64);
        let anyone = funded_operator(&mut self.svm, 1_000_000_000);
        send(&mut self.svm, ix_settle_market(&self.m.market, &self.m.vault, &self.m.oracle, &self.m.config, &anyone.pubkey()), &anyone, &[]).unwrap();
    }
}

fn fund_roots(svm: &mut litesvm::LiteSVM) {
    for i in 0..3 {
        svm.airdrop(&root_signer(i), 10_000_000_000).unwrap();
    }
    svm.airdrop(&guardian_signer(), 10_000_000_000).unwrap();
}

// ── Cancelling ───────────────────────────────────────────────────────────

#[test]
fn two_distinct_roots_can_cancel_but_one_key_or_an_outsider_cannot() {
    let mut w = world();
    fund_roots(&mut w.svm);
    let (r0, r1) = (root_signer(0), root_signer(1));
    let outsider = funded_keypair(&mut w.svm, 1_000_000_000).pubkey();

    assert_fails_with(w.cancel(Some((r0, r0))), MetisError::CancelNotAllowed);
    assert_fails_with(w.cancel(Some((r0, outsider))), MetisError::CancelNotAllowed);
    assert_fails_with(w.cancel(None), MetisError::CancelNotAllowed);
    assert_eq!(w.market().status, MarketStatus::Open as u8, "nothing applied");

    w.cancel(Some((r0, r1))).expect("two roots");
    let m = w.market();
    assert_eq!(m.status, MarketStatus::Cancelled as u8);
    assert_eq!(m.payout_pool, m.total_collected + m.fees_accrued, "everything in the vault goes back, fees included");
    assert_eq!((m.protocol_owed, m.creator_owed), (0, 0), "no fees are taken on a cancelled market");
}

#[test]
fn a_voided_oracle_lets_anyone_cancel_at_once() {
    let mut w = world();
    void_oracle(&mut w.svm, &w.m.oracle, w.m.bet_close_ts);
    w.cancel(None).expect("anyone, immediately");
    assert_eq!(w.market().status, MarketStatus::Cancelled as u8);
}

#[test]
fn with_no_result_anyone_may_cancel_only_after_the_overdue_deadline() {
    let mut w = world();
    let deadline = w.m.bet_close_ts + CANCEL_AFTER_SECS;
    set_clock(&mut w.svm, deadline - 1);
    assert_fails_with(w.cancel(None), MetisError::CancelNotAllowed);
    set_clock(&mut w.svm, deadline);
    w.cancel(None).expect("overdue: anyone");
    assert_eq!(w.market().status, MarketStatus::Cancelled as u8);
}

#[test]
fn an_oracle_that_has_been_closed_counts_as_no_result() {
    let mut w = world();
    w.svm.set_account(w.m.oracle, Account::default()).unwrap(); // closed: no data, system-owned
    let deadline = w.m.bet_close_ts + CANCEL_AFTER_SECS;
    set_clock(&mut w.svm, deadline - 1);
    assert_fails_with(w.cancel(None), MetisError::CancelNotAllowed);
    set_clock(&mut w.svm, deadline);
    w.cancel(None).expect("overdue and the oracle is gone");
}

#[test]
fn a_final_result_means_settle_so_only_two_roots_can_cancel_over_it() {
    let mut w = world();
    fund_roots(&mut w.svm);
    let at = w.m.bet_close_ts + 7_000;
    finalize_oracle(&mut w.svm, &w.m.oracle, 2, 0, at);
    set_clock(&mut w.svm, w.m.bet_close_ts + CANCEL_AFTER_SECS + 1);
    assert_fails_with(w.cancel(None), MetisError::CancelNotAllowed);
    w.cancel(World::two_roots()).expect("two roots still can");
}

#[test]
fn a_settled_market_cannot_be_cancelled_and_a_cancelled_one_cannot_be_settled() {
    let mut w = world();
    fund_roots(&mut w.svm);
    w.settle(2, 0);
    assert_fails_with(w.cancel(World::two_roots()), MetisError::InvalidStatusTransition);

    let mut c = world();
    fund_roots(&mut c.svm);
    c.cancel(World::two_roots()).unwrap();
    let at = c.m.bet_close_ts + 7_000;
    finalize_oracle(&mut c.svm, &c.m.oracle, 2, 0, at);
    set_clock(&mut c.svm, at + MIN_DISPUTE_SECS as i64);
    let anyone = funded_operator(&mut c.svm, 1_000_000_000);
    assert_fails_with(
        send(&mut c.svm, ix_settle_market(&c.m.market, &c.m.vault, &c.m.oracle, &c.m.config, &anyone.pubkey()), &anyone, &[]),
        MetisError::InvalidStatusTransition,
    );
}

// ── Refunding ────────────────────────────────────────────────────────────

#[test]
fn a_cancelled_market_refunds_every_holder_pro_rata_across_both_sides() {
    let mut w = world();
    fund_roots(&mut w.svm);
    let before_cancel = w.market();
    assert_fails_with(
        {
            let (kp, ata, side) = &w.bettors[0];
            send(&mut w.svm, ix_claim_refund(&w.m.market, &w.m.vault, &kp.pubkey(), ata, *side), kp, &[])
        },
        MetisError::MarketNotCancelled,
    );
    w.cancel(World::two_roots()).unwrap();
    let m = w.market();
    let total_shares: u64 = m.outcome_shares.iter().sum();
    let vault_before = token_balance(&w.svm, &w.m.vault);
    assert_eq!(vault_before, before_cancel.total_collected + before_cancel.fees_accrued);

    let mut refunded = 0u64;
    // Every bettor holds one side; the creator's seed holds both, so claims twice.
    let mut holders: Vec<(&Keypair, Pubkey, Vec<u8>)> = w.bettors.iter().map(|(kp, ata, side)| (kp, *ata, vec![*side])).collect();
    holders.push((&w.m.creator, w.m.creator_quote_ata, vec![0, 1]));
    for (kp, quote_ata, sides) in holders {
        for side in sides {
            let shares = token_balance(&w.svm, &outcome_ata(&w.m.market, &kp.pubkey(), side));
            let before = token_balance(&w.svm, &quote_ata);
            send(&mut w.svm, ix_claim_refund(&w.m.market, &w.m.vault, &kp.pubkey(), &quote_ata, side), kp, &[]).expect("refund");
            let got = token_balance(&w.svm, &quote_ata) - before;
            assert_eq!(got, compute_payout(shares, m.payout_pool, total_shares));
            refunded += got;
            assert_fails_with(
                send(&mut w.svm, ix_claim_refund(&w.m.market, &w.m.vault, &kp.pubkey(), &quote_ata, side), kp, &[]),
                MetisError::AlreadyClaimed,
            );
        }
    }

    // Everyone got their money back; only rounding dust is left.
    let dust = token_balance(&w.svm, &w.m.vault);
    assert_eq!(refunded + dust, vault_before);
    assert!(dust <= 10, "dust is a few atoms, got {dust}");
}

#[test]
fn a_refund_only_takes_this_markets_own_outcome_tokens_and_a_settled_market_does_not_refund() {
    let mut w = world();
    fund_roots(&mut w.svm);
    w.cancel(World::two_roots()).unwrap();
    let (kp, ata, _) = &w.bettors[0];
    let mut ix = ix_claim_refund(&w.m.market, &w.m.vault, &kp.pubkey(), ata, 0);
    ix.accounts[5].pubkey = usdc_mint(); // not an outcome mint of this market
    assert_fails_with(send(&mut w.svm, ix, kp, &[]), MetisError::InvalidPda);

    let mut s = world();
    s.settle(2, 0);
    let (kp, ata, side) = &s.bettors[0];
    assert_fails_with(
        send(&mut s.svm, ix_claim_refund(&s.m.market, &s.m.vault, &kp.pubkey(), ata, *side), kp, &[]),
        MetisError::MarketNotCancelled,
    );
}

#[test]
fn nothing_is_swept_from_a_cancelled_market() {
    let mut w = world();
    fund_roots(&mut w.svm);
    w.cancel(World::two_roots()).unwrap();
    let t_ata = treasury_ata(&mut w.svm);
    let anyone = funded_operator(&mut w.svm, 1_000_000_000);
    assert_fails_with(
        send(&mut w.svm, ix_sweep_fees(&w.m.market, &w.m.vault, &t_ata, &w.m.config, 0, &anyone.pubkey()), &anyone, &[]),
        MetisError::MarketNotSettled,
    );
}

// ── Closing a market for good ────────────────────────────────────────────

#[test]
fn a_market_closes_after_the_claim_window_once_the_creator_has_claimed_and_the_treasury_gets_the_rest() {
    let mut w = world();
    send(&mut w.svm, ix_set_metadata(&w.m.creator.pubkey(), &w.m.market, &Text::sample()), &w.m.creator, &[]).unwrap();
    w.settle(2, 0);
    let resolved = w.market().resolved_at;
    let t_ata = treasury_ata(&mut w.svm);
    let recipient = funded_operator(&mut w.svm, 1_000_000_000);
    let close = |w: &mut World| {
        send(&mut w.svm, ix_close_market(&w.m.market, &w.m.vault, &t_ata, &recipient.pubkey(), &w.m.config), &recipient, &[])
    };

    set_clock(&mut w.svm, resolved + CLAIM_WINDOW_SECS - 1);
    assert_fails_with(close(&mut w), MetisError::ClaimWindowOpen);

    // The creator's fees are theirs: closing waits for them to be claimed, however long that takes.
    let protocol_owed = w.market().protocol_owed;
    assert!(protocol_owed > 0 && w.market().creator_owed > 0);
    set_clock(&mut w.svm, resolved + CLAIM_WINDOW_SECS);
    assert_fails_with(close(&mut w), MetisError::FeesUnswept);

    // The creator claims. The protocol's share is NOT swept separately here: closing carries it.
    let anyone = funded_operator(&mut w.svm, 1_000_000_000);
    let creator_ata = w.m.creator_quote_ata;
    send(&mut w.svm, ix_sweep_fees(&w.m.market, &w.m.vault, &creator_ata, &w.m.config, 1, &anyone.pubkey()), &anyone, &[]).unwrap();
    assert_eq!(w.market().protocol_owed, protocol_owed, "still unswept");

    // One winner claimed in time; the rest of the winnings went unclaimed.
    let (kp, ata, _) = &w.bettors[0];
    send(&mut w.svm, ix_claim(&w.m.market, &w.m.vault, &kp.pubkey(), ata, 0), kp, &[]).unwrap();
    let left_in_vault = token_balance(&w.svm, &w.m.vault);
    let treasury_before = token_balance(&w.svm, &t_ata);
    let rent_before = w.svm.get_account(&recipient.pubkey()).unwrap().lamports;
    let rent_held: u64 = [w.m.market, w.m.vault, metadata_pda(&w.m.market).0]
        .iter()
        .map(|a| w.svm.get_account(a).map_or(0, |acc| acc.lamports))
        .sum();

    close(&mut w).expect("closes");

    assert_eq!(token_balance(&w.svm, &t_ata) - treasury_before, left_in_vault, "what was left goes to the treasury");
    assert!(left_in_vault >= protocol_owed, "and that includes the protocol's fees, with no separate sweep");
    for gone in [w.m.market, w.m.vault, metadata_pda(&w.m.market).0] {
        assert!(w.svm.get_account(&gone).is_none_or(|a| a.data.is_empty() && a.lamports == 0), "{gone} is closed");
    }
    let rent_after = w.svm.get_account(&recipient.pubkey()).unwrap().lamports;
    assert!(rent_after + 10_000 >= rent_before + rent_held, "the reclaimed rent goes to whoever ran it");

    // After that, nothing is claimable: the market no longer exists.
    let (kp, ata, _) = &w.bettors[2];
    assert!(send(&mut w.svm, ix_claim(&w.m.market, &w.m.vault, &kp.pubkey(), ata, 0), kp, &[]).is_err());
}

#[test]
fn the_creators_unclaimed_fees_keep_the_market_open_and_stay_claimable() {
    let mut w = world();
    w.settle(2, 0);
    let resolved = w.market().resolved_at;
    let t_ata = treasury_ata(&mut w.svm);
    let recipient = funded_operator(&mut w.svm, 1_000_000_000);
    let creator_owed = w.market().creator_owed;

    // A very long time later, nothing has been forfeited and closing still refuses.
    set_clock(&mut w.svm, resolved + 50 * CLAIM_WINDOW_SECS);
    assert_fails_with(
        send(&mut w.svm, ix_close_market(&w.m.market, &w.m.vault, &t_ata, &recipient.pubkey(), &w.m.config), &recipient, &[]),
        MetisError::FeesUnswept,
    );
    assert!(token_balance(&w.svm, &w.m.vault) >= creator_owed, "the fees are still in the vault");

    // The creator can still collect, late, and then the market closes.
    let anyone = funded_operator(&mut w.svm, 1_000_000_000);
    let creator_ata = w.m.creator_quote_ata;
    let before = token_balance(&w.svm, &creator_ata);
    send(&mut w.svm, ix_sweep_fees(&w.m.market, &w.m.vault, &creator_ata, &w.m.config, 1, &anyone.pubkey()), &anyone, &[]).unwrap();
    assert_eq!(token_balance(&w.svm, &creator_ata) - before, creator_owed);
    send(&mut w.svm, ix_close_market(&w.m.market, &w.m.vault, &t_ata, &recipient.pubkey(), &w.m.config), &recipient, &[]).expect("now it closes");
}

#[test]
fn a_market_with_no_metadata_still_closes() {
    let mut w = world();
    fund_roots(&mut w.svm);
    w.cancel(World::two_roots()).unwrap();
    let resolved = w.market().resolved_at;
    set_clock(&mut w.svm, resolved + CLAIM_WINDOW_SECS);
    let t_ata = treasury_ata(&mut w.svm);
    let recipient = funded_operator(&mut w.svm, 1_000_000_000);
    send(&mut w.svm, ix_close_market(&w.m.market, &w.m.vault, &t_ata, &recipient.pubkey(), &w.m.config), &recipient, &[]).expect("closes");
    assert!(!market_exists(&w.svm, &w.m.market));
}

#[test]
fn an_open_market_cannot_be_closed() {
    let mut w = world();
    let t_ata = treasury_ata(&mut w.svm);
    let recipient = funded_operator(&mut w.svm, 1_000_000_000);
    set_clock(&mut w.svm, w.m.bet_close_ts + 10 * CLAIM_WINDOW_SECS);
    assert_fails_with(
        send(&mut w.svm, ix_close_market(&w.m.market, &w.m.vault, &t_ata, &recipient.pubkey(), &w.m.config), &recipient, &[]),
        MetisError::MarketNotSettled,
    );
}

// ── Pausing ──────────────────────────────────────────────────────────────

#[test]
fn pausing_stops_what_moves_money_in_or_changes_state_but_never_traps_anyone() {
    let mut w = world();
    fund_roots(&mut w.svm);
    let root = root_signer(2);
    let outsider = funded_keypair(&mut w.svm, 1_000_000_000);
    assert!(send(&mut w.svm, ix_pause(&outsider.pubkey(), &w.m.config), &outsider, &[]).is_err(), "only a root can pause");
    send_as(&mut w.svm, ix_pause(&root, &w.m.config), root).expect("one root pauses");

    // Blocked: new markets, new bets, settling, sweeping.
    let (kp, ata, _) = &w.bettors[0];
    assert_fails_with(
        send(&mut w.svm, ix_place_bet(&w.m.market, &w.m.vault, &kp.pubkey(), ata, 0, 1_000_000, 0, &w.m.config), kp, &[]),
        MetisError::ProgramPaused,
    );
    let at = w.m.bet_close_ts + 7_000;
    finalize_oracle(&mut w.svm, &w.m.oracle, 2, 0, at);
    set_clock(&mut w.svm, at + MIN_DISPUTE_SECS as i64);
    let anyone = funded_operator(&mut w.svm, 1_000_000_000);
    assert_fails_with(
        send(&mut w.svm, ix_settle_market(&w.m.market, &w.m.vault, &w.m.oracle, &w.m.config, &anyone.pubkey()), &anyone, &[]),
        MetisError::ProgramPaused,
    );
    let kickoff = default_close_ts(&w.svm) + 100_000;
    let (creator, c_ata) = funded_bettor(&mut w.svm, 100_000_000);
    let oracle = oracle_account(&mut w.svm, kickoff);
    let (market, _) = market_pda(&creator.pubkey(), 9);
    assert_fails_with(
        send(&mut w.svm, ix_initialize_market(&creator.pubkey(), &c_ata, &market, &vault_pda(&market).0, 9, 2, HOME_WIN, 0, &w.m.config, &oracle), &creator, &[]),
        MetisError::ProgramPaused,
    );

    // Not blocked: the two-root emergency cancel, and the refunds that follow.
    w.cancel(World::two_roots()).expect("cancel works while paused");
    let (kp, ata, side) = &w.bettors[1];
    send(&mut w.svm, ix_claim_refund(&w.m.market, &w.m.vault, &kp.pubkey(), ata, *side), kp, &[]).expect("refunds work while paused");
}

#[test]
fn claims_work_while_paused() {
    let mut w = world();
    fund_roots(&mut w.svm);
    w.settle(2, 0);
    let root = root_signer(2);
    send_as(&mut w.svm, ix_pause(&root, &w.m.config), root).unwrap();
    let (kp, ata, _) = &w.bettors[0];
    let before = token_balance(&w.svm, ata);
    send(&mut w.svm, ix_claim(&w.m.market, &w.m.vault, &kp.pubkey(), ata, 0), kp, &[]).expect("a winner can always collect");
    assert!(token_balance(&w.svm, ata) > before);
}

#[test]
fn unpausing_needs_the_guardian_and_two_distinct_roots_and_restores_everything() {
    let mut w = world();
    fund_roots(&mut w.svm);
    let (g, r0, r1) = (guardian_signer(), root_signer(0), root_signer(1));
    send_as(&mut w.svm, ix_pause(&r0, &w.m.config), r0).unwrap();

    assert!(send_as(&mut w.svm, ix_unpause(&[g], &w.m.config), g).is_err(), "guardian alone");
    assert!(send_as(&mut w.svm, ix_unpause(&[r0, r1], &w.m.config), r0).is_err(), "roots without the guardian");
    assert!(send_as(&mut w.svm, ix_unpause(&[g, r0], &w.m.config), g).is_err(), "guardian and one root");
    assert!(send_as(&mut w.svm, ix_unpause(&[g, r0, r0], &w.m.config), g).is_err(), "one root twice counts once");

    let (kp, ata, _) = &w.bettors[0];
    assert!(send(&mut w.svm, ix_place_bet(&w.m.market, &w.m.vault, &kp.pubkey(), ata, 0, 1_000_000, 0, &w.m.config), kp, &[]).is_err(), "still paused");

    send_as(&mut w.svm, ix_unpause(&[g, r0, r1], &w.m.config), g).expect("guardian and two distinct roots");
    send(&mut w.svm, ix_place_bet(&w.m.market, &w.m.vault, &kp.pubkey(), ata, 0, 1_000_000, 0, &w.m.config), kp, &[]).expect("betting works again");
}

#[test]
fn attesting_a_new_authority_is_blocked_while_paused() {
    // Who is trusted must not change during an incident: attest and remove refuse while paused.
    let mut w = world();
    fund_roots(&mut w.svm);
    let r0 = root_signer(0);
    send_as(&mut w.svm, ix_pause(&r0, &w.m.config), r0).unwrap();

    let candidate = Pubkey::new_unique();
    let (authority_pda, _) = Pubkey::find_program_address(
        &[metis_core::constants::DELEGATED_AUTHORITY_SEED, candidate.as_ref()],
        &program_id(),
    );
    let ix = solana_sdk::instruction::Instruction {
        program_id: program_id(),
        accounts: vec![
            solana_sdk::instruction::AccountMeta::new(r0, true),
            solana_sdk::instruction::AccountMeta::new_readonly(w.m.config, false),
            solana_sdk::instruction::AccountMeta::new_readonly(candidate, false),
            solana_sdk::instruction::AccountMeta::new(authority_pda, false),
            solana_sdk::instruction::AccountMeta::new_readonly(solana_system_interface::program::ID, false),
        ],
        data: metis_core::config::state::ATTEST_AUTHORITY.to_vec(),
    };
    assert_fails_with(send_as(&mut w.svm, ix, r0), MetisError::ProgramPaused);
}
