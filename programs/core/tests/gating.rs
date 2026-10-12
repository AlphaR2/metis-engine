// Who may run the routine steps. Roots and attested operators may; everybody else may not - except
// where a fallback is meant to open to anyone (settle and cancel after a week), and the creator
// collecting their own fees. Forfeiting an unclaimed creator share needs two roots and 30 days.

mod common;

use common::*;
use metis_core::{
    config::state::ConfigDials,
    constants::{CREATOR_CLAIM_SECS, MIN_DISPUTE_SECS, OPERATOR_GRACE_SECS},
    error::MetisError,
};
use solana_sdk::{pubkey::Pubkey, signature::{Keypair, Signer}};

const DISPUTE: i64 = MIN_DISPUTE_SECS as i64;
const WEEK: i64 = OPERATOR_GRACE_SECS;

struct World {
    svm: litesvm::LiteSVM,
    m: TestMarket,
    finished_at: i64,
}

/// A market with fees and a rake and one bet on it; the oracle has a final result.
fn world() -> World {
    let mut svm = setup();
    set_clock(&mut svm, 1_000_000);
    let dials = ConfigDials { fee_bps: 50, rake_bps: 100, dispute_secs: MIN_DISPUTE_SECS, ..default_dials() };
    let config = setup_with_dials(&mut svm, dials);
    let kickoff = default_close_ts(&svm);
    let m = open_market_with_config(&mut svm, 0, kickoff, config);
    let (bettor, ata) = funded_bettor(&mut svm, 100_000_000);
    send(&mut svm, ix_place_bet(&m.market, &m.vault, &bettor.pubkey(), &ata, 0, 40_000_000, 0, &m.config), &bettor, &[]).unwrap();
    let finished_at = m.bet_close_ts + 7_000;
    finalize_oracle(&mut svm, &m.oracle, 1, 0, finished_at);
    World { svm, m, finished_at }
}

impl World {
    fn settle_with(&mut self, who: &Keypair) -> Result<(), litesvm::types::FailedTransactionMetadata> {
        let ix = ix_settle_market(&self.m.market, &self.m.vault, &self.m.oracle, &self.m.config, &who.pubkey());
        send(&mut self.svm, ix, who, &[])
    }

    fn settled(&mut self) {
        set_clock(&mut self.svm, self.finished_at + DISPUTE);
        let op = funded_operator(&mut self.svm, 1_000_000_000);
        self.settle_with(&op).unwrap();
    }

    fn sweep(&mut self, who: &Keypair, which: u8) -> Result<(), litesvm::types::FailedTransactionMetadata> {
        let ata = if which == 0 { treasury_ata(&mut self.svm) } else { self.m.creator_quote_ata };
        let ix = ix_sweep_fees(&self.m.market, &self.m.vault, &ata, &self.m.config, which, &who.pubkey());
        send(&mut self.svm, ix, who, &[])
    }

    fn forfeit(&mut self, first: Pubkey, second: Pubkey) -> Result<(), litesvm::types::FailedTransactionMetadata> {
        let t = treasury_ata(&mut self.svm);
        let ix = ix_forfeit_creator_fees(&self.m.market, &self.m.vault, &t, &self.m.config, &first, &second);
        send_as(&mut self.svm, ix, first)
    }
}

fn stranger(svm: &mut litesvm::LiteSVM) -> Keypair {
    funded_keypair(svm, 5_000_000_000)
}

#[test]
fn settle_is_for_operators_for_a_week_then_for_anyone() {
    let mut w = world();
    set_clock(&mut w.svm, w.finished_at + DISPUTE);
    let nobody = stranger(&mut w.svm);
    assert_fails_with(w.settle_with(&nobody), MetisError::OperatorRequired);

    // An operator whose attestation was removed is a stranger again.
    let gone = funded_operator(&mut w.svm, 1_000_000_000);
    revoke_operator(&mut w.svm, &gone.pubkey());
    assert_fails_with(w.settle_with(&gone), MetisError::OperatorRequired);

    // Still refused a second before the fallback opens...
    set_clock(&mut w.svm, w.finished_at + DISPUTE + WEEK - 1);
    assert_fails_with(w.settle_with(&nobody), MetisError::OperatorRequired);
    // ...and open to anyone the second it does.
    set_clock(&mut w.svm, w.finished_at + DISPUTE + WEEK);
    w.settle_with(&nobody).expect("anyone, after a week");
}

#[test]
fn an_attested_operator_settles_the_moment_the_dispute_window_ends() {
    let mut w = world();
    set_clock(&mut w.svm, w.finished_at + DISPUTE);
    let op = funded_operator(&mut w.svm, 1_000_000_000);
    w.settle_with(&op).expect("operator");
    assert_eq!(read_market(&w.svm, &w.m.market).resolved_at, w.finished_at + DISPUTE);
}

#[test]
fn a_root_is_an_operator_without_any_attestation() {
    let mut w = world();
    set_clock(&mut w.svm, w.finished_at + DISPUTE);
    let root = root_signer(2);
    w.svm.airdrop(&root, 1_000_000_000).unwrap();
    let ix = ix_settle_market(&w.m.market, &w.m.vault, &w.m.oracle, &w.m.config, &root);
    send_as(&mut w.svm, ix, root).expect("root settles");
}

#[test]
fn an_unsigned_operator_address_is_not_enough() {
    let mut w = world();
    set_clock(&mut w.svm, w.finished_at + DISPUTE);
    let op = funded_operator(&mut w.svm, 1_000_000_000);
    let payer = stranger(&mut w.svm);
    let mut ix = ix_settle_market(&w.m.market, &w.m.vault, &w.m.oracle, &w.m.config, &op.pubkey());
    ix.accounts[4].is_signer = false; // names the operator without their signature
    assert!(send(&mut w.svm, ix, &payer, &[]).is_err());
}

#[test]
fn closing_the_primary_window_is_for_operators_only() {
    let mut w = world();
    set_clock(&mut w.svm, primary_end_of(w.m.bet_close_ts) + 1);
    let nobody = stranger(&mut w.svm);
    assert_fails_with(
        send(&mut w.svm, ix_close_primary(&nobody.pubkey(), &w.m.market, &w.m.config), &nobody, &[]),
        MetisError::OperatorRequired,
    );
    // Not even a week later: nobody's money depends on the books existing.
    set_clock(&mut w.svm, primary_end_of(w.m.bet_close_ts) + 10 * WEEK);
    assert_fails_with(
        send(&mut w.svm, ix_close_primary(&nobody.pubkey(), &w.m.market, &w.m.config), &nobody, &[]),
        MetisError::OperatorRequired,
    );
    if load_manifest(&mut w.svm) {
        let op = funded_operator(&mut w.svm, 5_000_000_000);
        send(&mut w.svm, ix_close_primary(&op.pubkey(), &w.m.market, &w.m.config), &op, &[]).expect("operator");
    }
}

#[test]
fn the_protocol_sweep_is_gated_but_the_creators_is_not() {
    let mut w = world();
    w.settled();
    let nobody = stranger(&mut w.svm);
    assert_fails_with(w.sweep(&nobody, 0), MetisError::OperatorRequired);
    let before = token_balance(&w.svm, &w.m.creator_quote_ata);
    w.sweep(&nobody, 1).expect("the creator's money needs nobody's permission");
    assert!(token_balance(&w.svm, &w.m.creator_quote_ata) > before);
    let op = funded_operator(&mut w.svm, 1_000_000_000);
    w.sweep(&op, 0).expect("operator sweeps the protocol's share");
}

#[test]
fn closing_a_market_is_for_operators_only() {
    let mut w = world();
    w.settled();
    let op = funded_operator(&mut w.svm, 1_000_000_000);
    w.sweep(&op, 0).unwrap();
    w.sweep(&op, 1).unwrap();
    set_clock(&mut w.svm, w.finished_at + DISPUTE + metis_core::constants::CLAIM_WINDOW_SECS + 1);
    let t = treasury_ata(&mut w.svm);
    let nobody = stranger(&mut w.svm);
    assert_fails_with(
        send(&mut w.svm, ix_close_market(&w.m.market, &w.m.vault, &t, &nobody.pubkey(), &w.m.config), &nobody, &[]),
        MetisError::OperatorRequired,
    );
    send(&mut w.svm, ix_close_market(&w.m.market, &w.m.vault, &t, &op.pubkey(), &w.m.config), &op, &[]).expect("operator");
}

#[test]
fn cancel_needs_an_operator_until_a_week_after_it_became_possible() {
    let mut w = world();
    // Not cancellable at all by one signature while a final result stands.
    let nobody = stranger(&mut w.svm);
    let op = funded_operator(&mut w.svm, 1_000_000_000);
    let cancel = |w: &mut World, who: &Keypair| {
        let ix = ix_cancel_market(&w.m.market, &w.m.oracle, &w.m.config, &who.pubkey(), None);
        send(&mut w.svm, ix, who, &[])
    };
    assert_fails_with(cancel(&mut w, &op), MetisError::CancelNotAllowed);

    // Voided: an operator may cancel straight away, a stranger only after a week.
    void_oracle(&mut w.svm, &w.m.oracle, w.finished_at);
    set_clock(&mut w.svm, w.finished_at + 10);
    assert_fails_with(cancel(&mut w, &nobody), MetisError::OperatorRequired);
    set_clock(&mut w.svm, w.finished_at + WEEK);
    cancel(&mut w, &nobody).expect("anyone, a week after the void");

    let mut w = world();
    void_oracle(&mut w.svm, &w.m.oracle, w.finished_at);
    set_clock(&mut w.svm, w.finished_at + 10);
    let op = funded_operator(&mut w.svm, 1_000_000_000);
    let ix = ix_cancel_market(&w.m.market, &w.m.oracle, &w.m.config, &op.pubkey(), None);
    send(&mut w.svm, ix, &op, &[]).expect("operator cancels at once");
}

#[test]
fn forfeiting_unclaimed_creator_fees_needs_two_roots_and_thirty_days() {
    let mut w = world();
    let (r0, r1) = (root_signer(0), root_signer(1));
    w.svm.airdrop(&r0, 1_000_000_000).unwrap();

    // Not settled yet.
    assert_fails_with(w.forfeit(r0, r1), MetisError::MarketNotSettled);

    w.settled();
    let settled_at = read_market(&w.svm, &w.m.market).resolved_at;
    let owed = read_market(&w.svm, &w.m.market).creator_owed;
    assert!(owed > 0);

    set_clock(&mut w.svm, settled_at + CREATOR_CLAIM_SECS - 1);
    assert_fails_with(w.forfeit(r0, r1), MetisError::ForfeitTooEarly);

    set_clock(&mut w.svm, settled_at + CREATOR_CLAIM_SECS);
    assert_fails_with(w.forfeit(r0, r0), MetisError::TwoRootSignersRequired);
    let outsider = Pubkey::new_unique();
    assert_fails_with(w.forfeit(r0, outsider), MetisError::TwoRootSignersRequired);

    let t = treasury_ata(&mut w.svm);
    let before = token_balance(&w.svm, &t);
    let vault_before = token_balance(&w.svm, &w.m.vault);
    let m_before = read_market(&w.svm, &w.m.market);
    w.forfeit(r0, r1).expect("two distinct roots after 30 days");
    assert_eq!(token_balance(&w.svm, &t), before + owed, "the creator's share moved to the treasury");
    // Conservation: exactly that left the vault, and nothing else on the market's books moved.
    assert_eq!(token_balance(&w.svm, &w.m.vault), vault_before - owed);
    let m_after = read_market(&w.svm, &w.m.market);
    assert_eq!(m_after.payout_pool, m_before.payout_pool, "winners' pool untouched");
    assert_eq!(m_after.protocol_owed, m_before.protocol_owed, "protocol's share untouched");
    assert!(token_balance(&w.svm, &w.m.vault) >= m_after.payout_pool + m_after.protocol_owed, "the vault still covers every claim");

    // The record stays on the market for a manual settlement later.
    let m = read_market(&w.svm, &w.m.market);
    assert_eq!(m.forfeited_creator_fees, owed);
    assert_eq!(m.forfeited_at, settled_at + CREATOR_CLAIM_SECS);
    assert_eq!(m.creator_owed, 0);

    // Once. And the creator has nothing left to sweep.
    assert_fails_with(w.forfeit(r0, r1), MetisError::NothingToForfeit);
    let nobody = stranger(&mut w.svm);
    assert_fails_with(w.sweep(&nobody, 1), MetisError::NothingToSweep);
}

#[test]
fn a_creator_who_claims_in_time_leaves_nothing_to_forfeit() {
    let mut w = world();
    w.settled();
    let settled_at = read_market(&w.svm, &w.m.market).resolved_at;
    let nobody = stranger(&mut w.svm);
    w.sweep(&nobody, 1).unwrap();
    set_clock(&mut w.svm, settled_at + CREATOR_CLAIM_SECS);
    let r0 = root_signer(0);
    w.svm.airdrop(&r0, 1_000_000_000).unwrap();
    assert_fails_with(w.forfeit(r0, root_signer(1)), MetisError::NothingToForfeit);
}
