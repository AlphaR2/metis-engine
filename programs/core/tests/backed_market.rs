// A market seeded by a DBC backer raise instead of by its creator: Pending until an operator
// activates it, then an ordinary market whose seed shares sit in escrow for the backers to redeem.
//
// The DBC side (config, pool, graduation, migration, removing the partner liquidity) is proven
// against the real Meteora programs on a local validator, not here: these tests stand in for its
// result with what it leaves behind, a backer mint with no authorities and a funded vault.

mod common;

use common::*;
use metis_core::{
    amm::{curve::symmetric_seed, state::MarketStatus},
    config::state::ConfigDials,
    constants::{MIN_DISPUTE_SECS, MIN_RAISE_SECS, MIN_SEED, OUTCOME_UNRESOLVED},
    error::MetisError,
};
use solana_sdk::{pubkey::Pubkey, signature::{Keypair, Signer}};

const SEED: u64 = 2 * MIN_SEED; // 60 USDC
const T0: i64 = 1_000_000;
const SUPPLY: u64 = 1_000_000_000; // 1,000 backer tokens

struct Backed {
    svm: litesvm::LiteSVM,
    m: TestMarket,
    operator: Keypair,
    backer_mint: Pubkey,
    deadline: i64,
    /// (keypair, usdc account, tokens held). The last holder never redeems unless a test says so.
    holders: Vec<(Keypair, Pubkey, u64)>,
    platform: Pubkey,
}

fn dials() -> ConfigDials {
    ConfigDials {
        fee_bps: 50,
        rake_bps: 100,
        dispute_secs: MIN_DISPUTE_SECS,
        dbc_backing_enabled: 1,
        ..default_dials()
    }
}

/// A Pending backed market, a final backer mint, four holders (500 / 300 / 100 / 100 tokens). The
/// vault is still empty.
fn backed_with(d: ConfigDials) -> Backed {
    let mut svm = setup();
    set_clock(&mut svm, T0);
    let config = setup_with_dials(&mut svm, d);
    let operator = funded_operator(&mut svm, 10_000_000_000);
    let kickoff = default_close_ts(&svm);
    let oracle = oracle_account(&mut svm, kickoff);
    let backer_mint = Pubkey::new_unique();
    set_final_mint(&mut svm, &backer_mint, SUPPLY);
    let platform = Pubkey::new_unique();

    let creator = funded_keypair(&mut svm, 10_000_000_000);
    let creator_quote_ata = fund_usdc(&mut svm, &creator.pubkey(), 0);
    let (market, _) = market_pda(&creator.pubkey(), 0);
    let (vault, _) = vault_pda(&market);
    let deadline = T0 + MIN_RAISE_SECS + 600;
    send(
        &mut svm,
        ix_initialize_backed_market(
            &creator.pubkey(), &market, &vault, 0, metis_core::amm::condition::HOME_WIN, 0, &config, &oracle,
            platform.to_bytes(), &backer_mint, SEED, deadline,
        ),
        &creator,
        &[],
    )
    .expect("initialize_backed_market");

    let mut holders = Vec::new();
    for tokens in [500_000_000u64, 300_000_000, 100_000_000, 100_000_000] {
        let kp = funded_keypair(&mut svm, 10_000_000_000);
        let quote = fund_usdc(&mut svm, &kp.pubkey(), 0);
        set_token_balance(&mut svm, &backer_mint, &kp.pubkey(), tokens);
        holders.push((kp, quote, tokens));
    }
    let m = TestMarket { creator, creator_quote_ata, market, vault, config, oracle, bet_close_ts: kickoff };
    Backed { svm, m, operator, backer_mint, deadline, holders, platform }
}

fn backed() -> Backed {
    backed_with(dials())
}

impl Backed {
    fn fund_vault(&mut self, amount: u64) {
        set_vault_amount(&mut self.svm, &self.m.vault, amount);
    }

    fn activate(&mut self) -> Result<(), litesvm::types::FailedTransactionMetadata> {
        let ix = ix_activate_market(&self.m.market, &self.m.vault, &self.operator.pubkey(), &self.m.config, &self.backer_mint);
        send(&mut self.svm, ix, &self.operator, &[])
    }

    fn funded_and_active(&mut self) {
        self.fund_vault(SEED);
        self.activate().expect("activate");
    }

    fn market(&self) -> metis_core::amm::state::Market {
        read_market(&self.svm, &self.m.market)
    }

    fn vault_balance(&self) -> u64 {
        token_balance(&self.svm, &self.m.vault)
    }

    fn escrow(&self, outcome: u8) -> u64 {
        let (mint, _) = outcome_mint_pda(&self.m.market, outcome);
        token_balance(&self.svm, &ata(&self.m.market, &mint))
    }

    fn redeem(&mut self, holder: usize) -> Result<u64, litesvm::types::FailedTransactionMetadata> {
        let Backed { svm, m, holders, backer_mint, .. } = self;
        let (kp, quote, _) = &holders[holder];
        let before = token_balance(svm, quote);
        let ix = ix_claim_backer(&m.market, &m.vault, &kp.pubkey(), quote, backer_mint);
        send(svm, ix, kp, &[])?;
        Ok(token_balance(svm, quote) - before)
    }

    /// Settles the market: home wins 2-0, final at `at`, settled by the operator once disputes close.
    fn settle_home_win(&mut self) {
        let at = T0 + 100;
        finalize_oracle(&mut self.svm, &self.m.oracle, 2, 0, at);
        set_clock(&mut self.svm, at + MIN_DISPUTE_SECS as i64 + 1);
        let ix = ix_settle_market(&self.m.market, &self.m.vault, &self.m.oracle, &self.m.config, &self.operator.pubkey());
        send(&mut self.svm, ix, &self.operator, &[]).expect("settle");
    }
}

fn new_bettor(svm: &mut litesvm::LiteSVM, usdc: u64) -> (Keypair, Pubkey) {
    funded_bettor(svm, usdc)
}

// ── Creation ─────────────────────────────────────────────────────────────

#[test]
fn a_backed_market_is_born_pending_with_nothing_in_it() {
    let b = backed();
    let m = b.market();
    assert_eq!(m.status, MarketStatus::Pending as u8);
    assert_eq!(m.seed_source, metis_core::amm::state::SeedSource::DbcBackers as u8);
    assert_eq!(m.seed_target, SEED);
    assert_eq!(m.raise_deadline, b.deadline);
    assert_eq!(m.backer_mint.to_bytes(), b.backer_mint.to_bytes());
    assert_eq!(m.outcome_shares, [0, 0]);
    assert_eq!(m.total_collected, 0);
    assert_eq!(m.winning_outcome, OUTCOME_UNRESOLVED);
    assert_eq!(b.vault_balance(), 0);
    let (mint_0, _) = outcome_mint_pda(&b.m.market, 0);
    assert_eq!(mint_supply(&b.svm, &mint_0), 0, "no seed shares until activation");
}

#[test]
fn nobody_can_bet_on_a_pending_market() {
    let mut b = backed();
    let (kp, quote) = new_bettor(&mut b.svm, 100_000_000);
    let result = send(&mut b.svm, ix_place_bet(&b.m.market, &b.m.vault, &kp.pubkey(), &quote, 0, 10_000_000, 0, &b.m.config), &kp, &[]);
    assert_fails_with(result, MetisError::MarketNotOpen);
}

#[test]
fn backing_must_be_switched_on_and_the_raise_must_be_sane() {
    // Switched off: refused.
    let mut svm = setup();
    set_clock(&mut svm, T0);
    let config = setup_with_dials(&mut svm, ConfigDials { dbc_backing_enabled: 0, ..dials() });
    let kickoff = default_close_ts(&svm);
    let oracle = oracle_account(&mut svm, kickoff);
    let creator = funded_keypair(&mut svm, 10_000_000_000);
    let (market, _) = market_pda(&creator.pubkey(), 0);
    let (vault, _) = vault_pda(&market);
    let mint = Pubkey::new_unique();
    let make = |seed: u64, deadline: i64| {
        ix_initialize_backed_market(
            &creator.pubkey(), &market, &vault, 0, metis_core::amm::condition::HOME_WIN, 0, &config, &oracle, [0; 32], &mint, seed, deadline,
        )
    };
    assert_fails_with(send(&mut svm, make(SEED, T0 + MIN_RAISE_SECS + 60), &creator, &[]), MetisError::BackingDisabled);

    // Switched on: a seed under the minimum, a deadline too soon, too late, or after betting closes.
    let mut svm = setup();
    set_clock(&mut svm, T0);
    let config = setup_with_dials(&mut svm, dials());
    let kickoff = default_close_ts(&svm);
    let oracle = oracle_account(&mut svm, kickoff);
    let creator = funded_keypair(&mut svm, 10_000_000_000);
    let (market, _) = market_pda(&creator.pubkey(), 0);
    let (vault, _) = vault_pda(&market);
    let make = |seed: u64, deadline: i64, mint: Pubkey| {
        ix_initialize_backed_market(
            &creator.pubkey(), &market, &vault, 0, metis_core::amm::condition::HOME_WIN, 0, &config, &oracle, [0; 32], &mint, seed, deadline,
        )
    };
    let ok_deadline = T0 + MIN_RAISE_SECS + 60;
    assert_fails_with(send(&mut svm, make(MIN_SEED - 1, ok_deadline, mint), &creator, &[]), MetisError::ConfigValueOutOfBounds);
    assert_fails_with(send(&mut svm, make(SEED, T0 + 10, mint), &creator, &[]), MetisError::RaiseDeadlineInvalid);
    assert_fails_with(send(&mut svm, make(SEED, primary_end_of(kickoff) + 1, mint), &creator, &[]), MetisError::RaiseDeadlineInvalid);
    assert_fails_with(send(&mut svm, make(SEED, ok_deadline, Pubkey::default()), &creator, &[]), MetisError::BackerMintInvalid);
    send(&mut svm, make(SEED, ok_deadline, mint), &creator, &[]).expect("a sane raise is accepted");
}

// ── Activation ───────────────────────────────────────────────────────────

#[test]
fn activation_opens_the_market_and_escrows_the_seed_shares() {
    let mut b = backed();
    b.funded_and_active();
    let m = b.market();
    let per_side = symmetric_seed(SEED, 2);
    assert_eq!(m.status, MarketStatus::Open as u8);
    assert_eq!(m.outcome_shares, [per_side, per_side]);
    assert_eq!(m.total_collected, SEED);
    assert_eq!(m.backer_supply, SUPPLY);
    assert_eq!(b.escrow(0), per_side);
    assert_eq!(b.escrow(1), per_side);
    let (mint_0, _) = outcome_mint_pda(&b.m.market, 0);
    assert_eq!(mint_supply(&b.svm, &mint_0), per_side, "the only shares are the escrowed seed");

    // And it now behaves like any market.
    let (kp, quote) = new_bettor(&mut b.svm, 100_000_000);
    let got = bet(&mut b.svm, &b.m, &kp, &quote, 0, 20_000_000);
    assert!(got > 0);
}

#[test]
fn only_an_operator_may_activate() {
    let mut b = backed();
    b.fund_vault(SEED);
    let stranger = funded_keypair(&mut b.svm, 10_000_000_000);
    let ix = ix_activate_market(&b.m.market, &b.m.vault, &stranger.pubkey(), &b.m.config, &b.backer_mint);
    assert_fails_with(send(&mut b.svm, ix, &stranger, &[]), MetisError::OperatorRequired);
    assert_eq!(b.market().status, MarketStatus::Pending as u8, "nothing applied");
}

#[test]
fn activation_needs_the_seed_in_the_vault() {
    let mut b = backed();
    b.fund_vault(SEED - 1);
    assert_fails_with(b.activate(), MetisError::SeedNotFunded);
    b.fund_vault(SEED);
    b.activate().expect("funded");
}

#[test]
fn activation_refuses_a_backer_mint_that_can_still_mint_or_is_not_the_recorded_one() {
    // Still has a mint authority: the supply is not fixed, so a claim on the seed could be inflated.
    let mut b = backed();
    b.fund_vault(SEED);
    let authority = Pubkey::new_unique();
    set_mint_with_authority(&mut b.svm, &b.backer_mint, &authority, SUPPLY);
    assert_fails_with(b.activate(), MetisError::BackerMintInvalid);

    // No supply at all.
    let mut b = backed();
    b.fund_vault(SEED);
    set_final_mint(&mut b.svm, &b.backer_mint, 0);
    assert_fails_with(b.activate(), MetisError::BackerMintInvalid);

    // A different, perfectly good mint than the one the market recorded.
    let mut b = backed();
    b.fund_vault(SEED);
    let other = Pubkey::new_unique();
    set_final_mint(&mut b.svm, &other, SUPPLY);
    let ix = ix_activate_market(&b.m.market, &b.m.vault, &b.operator.pubkey(), &b.m.config, &other);
    assert_fails_with(send(&mut b.svm, ix, &b.operator, &[]), MetisError::BackerMintInvalid);
}

#[test]
fn activation_after_the_deadline_is_refused_and_it_cannot_run_twice() {
    let mut b = backed();
    b.fund_vault(SEED);
    set_clock(&mut b.svm, b.deadline + 1);
    assert_fails_with(b.activate(), MetisError::RaiseDeadlinePassed);

    let mut b = backed();
    b.funded_and_active();
    assert_fails_with(b.activate(), MetisError::InvalidStatusTransition);
}

// ── A raise that fails ───────────────────────────────────────────────────

#[test]
fn a_raise_that_misses_its_deadline_can_be_cancelled_by_anyone() {
    let mut b = backed();
    let stranger = funded_keypair(&mut b.svm, 10_000_000_000);
    let ix = || ix_cancel_pending_market(&b.m.market, &b.m.config, &stranger.pubkey());

    // Before the deadline a stranger can't give up on it.
    let early = ix();
    assert_fails_with(send(&mut b.svm, early, &stranger, &[]), MetisError::RaiseNotExpired);
    assert_eq!(b.market().status, MarketStatus::Pending as u8);

    set_clock(&mut b.svm, b.deadline);
    let late = ix_cancel_pending_market(&b.m.market, &b.m.config, &stranger.pubkey());
    send(&mut b.svm, late, &stranger, &[]).expect("anyone may cancel once the deadline has passed");
    let m = b.market();
    assert_eq!(m.status, MarketStatus::Cancelled as u8);
    assert_eq!(m.payout_pool, 0);
    assert_eq!(b.vault_balance(), 0, "nothing was ever held, so nothing is owed");

    // It cannot then be activated.
    b.fund_vault(SEED);
    assert_fails_with(b.activate(), MetisError::InvalidStatusTransition);
}

#[test]
fn an_operator_may_give_up_on_a_raise_early() {
    let mut b = backed();
    let ix = ix_cancel_pending_market(&b.m.market, &b.m.config, &b.operator.pubkey());
    send(&mut b.svm, ix, &b.operator, &[]).expect("operator cancels before the deadline");
    assert_eq!(b.market().status, MarketStatus::Cancelled as u8);
}

// ── Settling and redeeming ───────────────────────────────────────────────

#[test]
fn every_backer_redeems_their_slice_and_the_vault_adds_up_to_the_cent() {
    let mut b = backed();
    b.funded_and_active();

    // Two bettors, one each side. YES (home win) will win.
    let (yes, yes_q) = new_bettor(&mut b.svm, 1_000_000_000);
    let (no, no_q) = new_bettor(&mut b.svm, 1_000_000_000);
    bet(&mut b.svm, &b.m, &yes, &yes_q, 0, 40_000_000);
    bet(&mut b.svm, &b.m, &no, &no_q, 1, 30_000_000);
    let vault_in = b.vault_balance();
    assert_eq!(vault_in, SEED + 70_000_000, "seed plus both bets, fees included");

    b.settle_home_win();
    let m = b.market();
    assert_eq!(m.status, MarketStatus::Settled as u8);
    assert!(m.backers_owed > 0, "a backed market pays the backers their share of the revenue");
    assert_eq!(m.winning_outcome, 0);
    // The settlement's obligations fit in the vault.
    let obligations = m.payout_pool + m.protocol_owed + m.creator_owed + m.platform_owed + m.backers_owed;
    assert!(obligations <= vault_in, "solvent: {obligations} <= {vault_in}");

    // The bettors who backed the winner claim as usual.
    let before = token_balance(&b.svm, &yes_q);
    send(&mut b.svm, ix_claim(&b.m.market, &b.m.vault, &yes.pubkey(), &yes_q, 0), &yes, &[]).expect("winner claims");
    let yes_paid = token_balance(&b.svm, &yes_q) - before;

    // The first three holders redeem; the 500 : 300 : 100 token split shows in what they get.
    let a = b.redeem(0).expect("backer 0");
    let c = b.redeem(1).expect("backer 1");
    let d = b.redeem(2).expect("backer 2");
    assert!(a > 0 && c > 0 && d > 0);
    assert!(a.abs_diff(c * 5 / 3) <= 3, "500 vs 300 tokens: {a} vs {c}");
    assert!(a.abs_diff(d * 5) <= 6, "500 vs 100 tokens: {a} vs {d}");
    // Redeeming twice finds nothing left.
    assert_fails_with(b.redeem(0).map(|_| ()), MetisError::NothingToRedeem);
    // The fourth holder redeems last and gets the same per token as the first.
    let last = b.redeem(3).expect("backer 3");
    assert!(last.abs_diff(d) <= 3, "the last 100 tokens are worth what the earlier 100 were: {last} vs {d}");

    // Everyone is out. The vault holds exactly what the protocol, creator and platform are still
    // owed, plus rounding dust, and the backers took no more than their share.
    let owed_out = m.protocol_owed + m.creator_owed + m.platform_owed;
    let left = b.vault_balance();
    assert!(left >= owed_out, "the sweeps are still covered: {left} >= {owed_out}");
    assert!(left - owed_out <= 40, "only dust beyond that: {}", left - owed_out);
    assert_eq!(left + yes_paid + a + c + d + last, vault_in, "every atom accounted for");
    assert_eq!(b.market().backers_owed, 0, "the backers' revenue share is fully paid");
}

#[test]
fn redeeming_before_settlement_is_refused() {
    let mut b = backed();
    b.funded_and_active();
    assert_fails_with(b.redeem(0).map(|_| ()), MetisError::MarketNotSettled);
}

#[test]
fn redeeming_with_no_backer_tokens_is_refused() {
    let mut b = backed();
    b.funded_and_active();
    b.settle_home_win();
    // A holder who never held any.
    let nobody = funded_keypair(&mut b.svm, 10_000_000_000);
    let quote = fund_usdc(&mut b.svm, &nobody.pubkey(), 0);
    set_token_balance(&mut b.svm, &b.backer_mint, &nobody.pubkey(), 0);
    let ix = ix_claim_backer(&b.m.market, &b.m.vault, &nobody.pubkey(), &quote, &b.backer_mint);
    assert_fails_with(send(&mut b.svm, ix, &nobody, &[]), MetisError::NothingToRedeem);
}

#[test]
fn a_creator_seeded_market_has_no_backers_to_redeem() {
    // An ordinary market on a config with backing switched on.
    let mut svm = setup();
    set_clock(&mut svm, T0);
    let config = setup_with_dials(&mut svm, dials());
    let kickoff = default_close_ts(&svm);
    let m = open_market_with_config(&mut svm, 0, kickoff, config);
    let backer_mint = Pubkey::new_unique();
    set_final_mint(&mut svm, &backer_mint, SUPPLY);
    let holder = funded_keypair(&mut svm, 10_000_000_000);
    let quote = fund_usdc(&mut svm, &holder.pubkey(), 0);
    set_token_balance(&mut svm, &backer_mint, &holder.pubkey(), 1_000);
    let ix = ix_claim_backer(&m.market, &m.vault, &holder.pubkey(), &quote, &backer_mint);
    assert_fails_with(send(&mut svm, ix, &holder, &[]), MetisError::NotBackerMarket);
}

// ── A market that is cancelled after it opened ───────────────────────────

#[test]
fn a_cancelled_backed_market_refunds_bettors_and_backers_with_nothing_lost() {
    let mut b = backed();
    b.funded_and_active();
    let (yes, yes_q) = new_bettor(&mut b.svm, 1_000_000_000);
    let (no, no_q) = new_bettor(&mut b.svm, 1_000_000_000);
    bet(&mut b.svm, &b.m, &yes, &yes_q, 0, 40_000_000);
    bet(&mut b.svm, &b.m, &no, &no_q, 1, 30_000_000);
    let vault_in = b.vault_balance();

    // The match is voided; an operator cancels.
    let at = T0 + 100;
    void_oracle(&mut b.svm, &b.m.oracle, at);
    set_clock(&mut b.svm, at + 10);
    let ix = ix_cancel_market(&b.m.market, &b.m.oracle, &b.m.config, &b.operator.pubkey(), None);
    send(&mut b.svm, ix, &b.operator, &[]).expect("cancel");
    assert_eq!(b.market().status, MarketStatus::Cancelled as u8);

    // Both bettors take their refunds.
    let (b0, b1) = (token_balance(&b.svm, &yes_q), token_balance(&b.svm, &no_q));
    send(&mut b.svm, ix_claim_refund(&b.m.market, &b.m.vault, &yes.pubkey(), &yes_q, 0), &yes, &[]).expect("refund yes");
    send(&mut b.svm, ix_claim_refund(&b.m.market, &b.m.vault, &no.pubkey(), &no_q, 1), &no, &[]).expect("refund no");
    let bettors_paid = (token_balance(&b.svm, &yes_q) - b0) + (token_balance(&b.svm, &no_q) - b1);

    // All four backer holders redeem the escrowed seed.
    let mut backers_paid = 0;
    for i in 0..4 {
        backers_paid += b.redeem(i).expect("backer refund");
    }
    let left = b.vault_balance();
    assert_eq!(left + bettors_paid + backers_paid, vault_in, "every atom accounted for");
    assert!(left <= 40, "only dust is left: {left}");
    // Backers got back (almost) the seed they put in, bettors (almost) what they staked.
    assert!(backers_paid >= SEED - 40, "backers recover the seed: {backers_paid}");
}

#[test]
fn a_backed_market_that_never_settles_cannot_strand_the_backers() {
    // Cancelled with no bets at all: the backers still get the whole seed back.
    let mut b = backed();
    b.funded_and_active();
    let at = T0 + 100;
    void_oracle(&mut b.svm, &b.m.oracle, at);
    set_clock(&mut b.svm, at + 10);
    let ix = ix_cancel_market(&b.m.market, &b.m.oracle, &b.m.config, &b.operator.pubkey(), None);
    send(&mut b.svm, ix, &b.operator, &[]).expect("cancel");
    let mut paid = 0;
    for i in 0..4 {
        paid += b.redeem(i).expect("backer refund");
    }
    assert!(paid >= SEED - 10 && paid <= SEED, "{paid}");
}

// ── The platform leg still works on a backed market ──────────────────────

#[test]
fn the_platform_creator_and_protocol_are_paid_alongside_the_backers() {
    let mut b = backed();
    b.funded_and_active();
    let (yes, yes_q) = new_bettor(&mut b.svm, 1_000_000_000);
    bet(&mut b.svm, &b.m, &yes, &yes_q, 0, 50_000_000);
    b.settle_home_win();
    let m = b.market();
    // Creator-seeded markets fold the backers' share into the creator; a backed one keeps it apart.
    assert!(m.backers_owed > 0);
    assert!(m.creator_owed > 0);
    assert!(m.platform_owed > 0, "the platform named at creation earns its cut");
    assert_eq!(m.platform.to_bytes(), b.platform.to_bytes());
}
