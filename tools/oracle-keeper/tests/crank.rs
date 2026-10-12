//! The crank against the real compiled programs (core and Manifest, in litesvm): a market walked
//! through its whole life by crank passes alone, and the rules at each edge.
//!
//! Needs `target/deploy/metis_core.so` and `manifest.so`; skips loudly without them.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use litesvm::LiteSVM;
use metis_core::{
    amm::{
        condition,
        curve::{symmetric_seed, virtual_liquidity_per_side},
        state::{MARKET_DISCRIMINATOR, Market, MarketStatus},
    },
    config::state::{CONFIG_DISCRIMINATOR, Config},
    constants::{CANCEL_AFTER_SECS, CLAIM_WINDOW_SECS, MARKET_SEED, OUTCOME_MINT_SEED, OUTCOME_UNRESOLVED},
    settlement::{instructions::sweep_fees::SweepFeesArgs, state::SWEEP_FEES},
};
use oracle_keeper::{
    chain::{Chain, ChainError, decode_oracle},
    crank::{Action, Crank, Deployment, Report, associated_token_account},
    model::{FootballSettled, Phase},
};
use solana_account::Account;
use solana_clock::Clock;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

const T0: i64 = 1_000_000;
const KICKOFF: i64 = T0 + 24 * 3600;
const PRIMARY_END: i64 = KICKOFF - 5 * 3600;
const DISPUTE: i64 = 900;
const TOKEN: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

/// The program inside litesvm, behind the same `Chain` the crank uses against an RPC node.
#[derive(Clone)]
struct LocalChain {
    svm: Arc<Mutex<LiteSVM>>,
    /// litesvm can't list a program's accounts, so the test registers the ones it creates; the
    /// crank's size-and-discriminator filter is still applied to their real bytes.
    known: Arc<Mutex<Vec<Pubkey>>>,
}

#[async_trait]
impl Chain for LocalChain {
    async fn oracle(&self, address: &Pubkey) -> Result<Option<FootballSettled>, ChainError> {
        let svm = self.svm.lock().unwrap();
        svm.get_account(address).map(|a| decode_oracle(address, &a.data)).transpose()
    }

    async fn account(&self, address: &Pubkey) -> Result<Option<Vec<u8>>, ChainError> {
        Ok(self.svm.lock().unwrap().get_account(address).filter(|a| a.lamports > 0).map(|a| a.data))
    }

    async fn program_accounts(
        &self,
        _program: &Pubkey,
        data_size: usize,
        offset: usize,
        discriminator: &[u8; 8],
    ) -> Result<Vec<(Pubkey, Vec<u8>)>, ChainError> {
        let svm = self.svm.lock().unwrap();
        Ok(self
            .known
            .lock()
            .unwrap()
            .iter()
            .filter_map(|a| svm.get_account(a).map(|acc| (*a, acc.data)))
            .filter(|(_, d)| d.len() == data_size && d.get(offset..offset + 8) == Some(&discriminator[..]))
            .collect())
    }

    async fn now(&self) -> Result<i64, ChainError> {
        Ok(self.svm.lock().unwrap().get_sysvar::<Clock>().unix_timestamp)
    }

    async fn balance(&self, address: &Pubkey) -> Result<u64, ChainError> {
        Ok(self.svm.lock().unwrap().get_balance(address).unwrap_or(0))
    }

    async fn send(&self, payer: &Keypair, ixs: &[Instruction]) -> Result<String, ChainError> {
        let mut svm = self.svm.lock().unwrap();
        svm.expire_blockhash();
        let blockhash = svm.latest_blockhash();
        let message = Message::new_with_blockhash(ixs, Some(&payer.pubkey()), &blockhash);
        svm.send_transaction(Transaction::new(&[payer], message, blockhash))
            .map(|m| m.signature.to_string())
            .map_err(|e| ChainError::Rpc(format!("{:?}", e.err)))
    }
}

fn pack_mint(authority: Option<&Pubkey>, supply: u64) -> Vec<u8> {
    let mut d = vec![0u8; 82];
    if let Some(a) = authority {
        d[0..4].copy_from_slice(&1u32.to_le_bytes());
        d[4..36].copy_from_slice(a.as_ref());
        d[46..50].copy_from_slice(&1u32.to_le_bytes());
        d[50..82].copy_from_slice(a.as_ref());
    }
    d[36..44].copy_from_slice(&supply.to_le_bytes());
    d[44] = 6;
    d[45] = 1;
    d
}

fn pack_token(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Vec<u8> {
    let mut d = vec![0u8; 165];
    d[0..32].copy_from_slice(mint.as_ref());
    d[32..64].copy_from_slice(owner.as_ref());
    d[64..72].copy_from_slice(&amount.to_le_bytes());
    d[108] = 1;
    d
}

/// An operator's attestation account as two roots completing it would leave it (or, with
/// `complete = false`, one that was never finished).
fn write_attestation(svm: &mut LiteSVM, program: &Pubkey, operator: &Pubkey, complete: bool) {
    let mut d = vec![0u8; 112];
    d[0..32].copy_from_slice(operator.as_ref());
    d[32..64].copy_from_slice(&[1; 32]);
    d[64..96].copy_from_slice(&[2; 32]);
    d[96..104].copy_from_slice(&(complete as u64).to_le_bytes());
    d[104..112].copy_from_slice(&metis_core::config::state::DELEGATED_AUTHORITY_DISCRIMINATOR);
    let address = oracle_keeper::crank::delegate_of(operator, program);
    svm.set_account(address, Account { lamports: 10_000_000, data: d, owner: *program, ..Default::default() }).unwrap();
}

struct World {
    chain: LocalChain,
    crank: Crank<LocalChain>,
    program: Pubkey,
    quote_mint: Pubkey,
    treasury: Pubkey,
}

impl World {
    /// `None` (after saying so) if the compiled programs aren't there.
    fn new() -> Option<Self> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/deploy");
        let program = Pubkey::new_from_array(metis_core::ID.to_bytes());
        let manifest = Pubkey::new_from_array(metis_core::constants::MANIFEST_PROGRAM_ID.to_bytes());
        let mut svm = LiteSVM::new();
        if svm.add_program_from_file(program, root.join("metis_core.so")).is_err() || svm.add_program_from_file(manifest, root.join("manifest.so")).is_err() {
            eprintln!("SKIPPED: build the programs (`cargo build-sbf --manifest-path programs/core/Cargo.toml`) and fetch Manifest (`just fetch-manifest`)");
            return None;
        }
        let mut clock = svm.get_sysvar::<Clock>();
        clock.unix_timestamp = T0;
        svm.set_sysvar(&clock);

        let deployment = Deployment::for_this_build();
        let quote_mint = Pubkey::new_unique();
        let treasury = Pubkey::new_unique();
        svm.set_account(quote_mint, Account { lamports: 10_000_000, data: pack_mint(None, 0), owner: TOKEN, ..Default::default() }).unwrap();

        // The Config account, laid out by hand: only the treasury and discriminator matter here.
        let mut config = vec![0u8; core::mem::size_of::<Config>()];
        let at = core::mem::offset_of!(Config, treasury);
        config[at..at + 32].copy_from_slice(treasury.as_ref());
        let at = core::mem::offset_of!(Config, discriminator);
        config[at..at + 8].copy_from_slice(&CONFIG_DISCRIMINATOR);
        svm.set_account(deployment.config, Account { lamports: 10_000_000, data: config, owner: program, ..Default::default() }).unwrap();

        let payer = Keypair::new_from_array([66; 32]);
        svm.airdrop(&payer.pubkey(), 100_000_000_000).unwrap();
        // The crank key is an attested operator, as it must be on a real deployment.
        write_attestation(&mut svm, &program, &payer.pubkey(), true);
        let chain = LocalChain { svm: Arc::new(Mutex::new(svm)), known: Arc::default() };
        let crank = Crank::new(chain.clone(), payer, deployment);
        Some(Self { chain, crank, program, quote_mint, treasury })
    }

    fn set_time(&self, ts: i64) {
        let mut svm = self.chain.svm.lock().unwrap();
        let mut clock = svm.get_sysvar::<Clock>();
        clock.unix_timestamp = ts;
        svm.set_sysvar(&clock);
    }

    /// A market in `status`, with its vault, outcome mints and an oracle, all as the program would
    /// have left them. `edit` adjusts the market before it is written.
    fn add_market(&self, nonce: u64, status: MarketStatus, edit: impl FnOnce(&mut Market)) -> (Pubkey, Pubkey) {
        let creator = Pubkey::new_unique();
        let (address, bump) = Pubkey::find_program_address(&[MARKET_SEED, creator.as_ref(), &nonce.to_le_bytes()], &self.program);
        let oracle = Pubkey::new_unique();
        let vault = Pubkey::new_unique();

        let seed_each = symmetric_seed(30_000_000, 2);
        let mut m = Market {
            outcome_shares: [seed_each, seed_each],
            virtual_per_side: virtual_liquidity_per_side(seed_each, 30).unwrap(),
            total_collected: 30_000_000,
            fees_accrued: 150_000,
            bet_close_ts: KICKOFF,
            primary_end: PRIMARY_END,
            nonce,
            dispute_secs: DISPUTE as u32,
            fee_bps: 50,
            rake_bps: 100,
            fee_share_protocol_bps: 4_000,
            fee_share_backers_bps: 3_000,
            fee_share_creator_bps: 2_000,
            fee_share_platform_bps: 1_000,
            creator: creator.to_bytes().into(),
            oracle: oracle.to_bytes().into(),
            vault: vault.to_bytes().into(),
            quote_mint: self.quote_mint.to_bytes().into(),
            discriminator: MARKET_DISCRIMINATOR,
            num_outcomes: 2,
            status: status as u8,
            winning_outcome: OUTCOME_UNRESOLVED,
            bump,
            condition_kind: condition::HOME_WIN,
            ..Default::default()
        };
        edit(&mut m);

        let mut svm = self.chain.svm.lock().unwrap();
        let acct = |data: Vec<u8>, owner: Pubkey| Account { lamports: 10_000_000, data, owner, ..Default::default() };
        svm.set_account(address, acct(bytemuck::bytes_of(&m).to_vec(), self.program)).unwrap();
        svm.set_account(vault, acct(pack_token(&self.quote_mint, &address, m.total_collected + m.fees_accrued), TOKEN)).unwrap();
        for side in 0..2u8 {
            let (mint, _) = Pubkey::find_program_address(&[OUTCOME_MINT_SEED, address.as_ref(), &[side]], &self.program);
            svm.set_account(mint, acct(pack_mint(Some(&address), m.outcome_shares[side as usize]), TOKEN)).unwrap();
        }
        let fixture = FootballSettled::new(oracle.to_bytes(), KICKOFF, T0);
        svm.set_account(oracle, acct(fixture.as_bytes().to_vec(), Pubkey::new_from_array(oracle::ID.to_bytes()))).unwrap();
        drop(svm);
        self.chain.known.lock().unwrap().push(address);
        (address, oracle)
    }

    fn market(&self, address: &Pubkey) -> Market {
        let data = self.chain.svm.lock().unwrap().get_account(address).expect("market exists").data;
        bytemuck::pod_read_unaligned(&data)
    }

    fn exists(&self, address: &Pubkey) -> bool {
        self.chain.svm.lock().unwrap().get_account(address).is_some_and(|a| a.lamports > 0 && !a.data.is_empty())
    }

    fn edit_oracle(&self, address: &Pubkey, edit: impl FnOnce(&mut FootballSettled)) {
        let mut svm = self.chain.svm.lock().unwrap();
        let mut account = svm.get_account(address).unwrap();
        let mut o = decode_oracle(address, &account.data).unwrap();
        edit(&mut o);
        account.data = o.as_bytes().to_vec();
        svm.set_account(*address, account).unwrap();
    }

    fn finalize(&self, oracle: &Pubkey, home: u8, away: u8, at: i64) {
        self.edit_oracle(oracle, |o| {
            let score = FootballSettled::pack_score(home, away);
            o.status = Phase::Finished;
            o.score = score;
            o.regulation_score = score;
            o.finalized = 1;
            o.finished_at = at.to_le_bytes();
        });
    }

    fn token_balance(&self, address: &Pubkey) -> u64 {
        let svm = self.chain.svm.lock().unwrap();
        svm.get_account(address).map(|a| u64::from_le_bytes(a.data[64..72].try_into().unwrap())).unwrap_or(0)
    }

    async fn pass(&self) -> Vec<Report> {
        self.crank.run_once().await.expect("the pass itself works")
    }
}

fn actions(reports: &[Report]) -> Vec<Action> {
    reports.iter().map(|r| r.action).collect()
}

fn all_ok(reports: &[Report]) {
    for r in reports {
        assert!(r.outcome.is_ok(), "{:?} on {} failed: {:?}", r.action, r.market, r.outcome);
    }
}

// ── A whole life ─────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn a_market_is_walked_through_its_whole_life_by_the_crank_alone() {
    let Some(w) = World::new() else { return };
    let (market, oracle) = w.add_market(1, MarketStatus::Open, |_| {});

    // Betting is open: nothing is due.
    w.set_time(T0 + 60);
    assert!(w.pass().await.is_empty());

    // Betting ends: the crank opens the order books, once.
    w.set_time(PRIMARY_END);
    let reports = w.pass().await;
    assert_eq!(actions(&reports), vec![Action::ClosePrimary]);
    all_ok(&reports);
    assert_eq!(w.market(&market).status, MarketStatus::Closed as u8);
    assert!(w.pass().await.is_empty(), "a second pass finds nothing left to do");

    // The match finishes. Inside this market's dispute window the crank waits.
    let at = KICKOFF + 7_000;
    w.finalize(&oracle, 2, 0, at);
    w.set_time(at + DISPUTE - 1);
    assert!(w.pass().await.is_empty());

    // The window closes: it settles, home win, so YES (outcome 0).
    w.set_time(at + DISPUTE);
    let reports = w.pass().await;
    assert_eq!(actions(&reports), vec![Action::Settle]);
    all_ok(&reports);
    let settled = w.market(&market);
    assert_eq!((settled.status, settled.winning_outcome), (MarketStatus::Settled as u8, 0));
    let (protocol_owed, creator_owed) = (settled.protocol_owed, settled.creator_owed);
    assert!(protocol_owed > 0 && creator_owed > 0);

    // Next: the protocol's share goes to the treasury - the crank creating its token account first.
    let treasury_ata = associated_token_account(&w.treasury, &w.quote_mint);
    assert_eq!(w.token_balance(&treasury_ata), 0);
    let reports = w.pass().await;
    assert_eq!(actions(&reports), vec![Action::SweepProtocol]);
    all_ok(&reports);
    assert_eq!(w.token_balance(&treasury_ata), protocol_owed);
    assert_eq!(w.market(&market).protocol_owed, 0);

    // The creator's share is NOT paid by the crank: it stays for them.
    assert!(w.pass().await.is_empty());
    assert_eq!(w.market(&market).creator_owed, creator_owed);

    // After the claim window the market still stays open for as long as the creator hasn't claimed.
    w.set_time(settled.resolved_at + CLAIM_WINDOW_SECS + 1_000_000);
    assert!(w.pass().await.is_empty());
    assert!(w.exists(&market));
    assert!(w.token_balance(&Pubkey::new_from_array(settled.vault.to_bytes())) >= creator_owed, "their fees are still there");

    // The creator claims (their own transaction, not the crank's)...
    let creator = Pubkey::new_from_array(settled.creator.to_bytes());
    let creator_ata = associated_token_account(&creator, &w.quote_mint);
    w.chain.svm.lock().unwrap().set_account(creator_ata, Account { lamports: 10_000_000, data: pack_token(&w.quote_mint, &creator, 0), owner: TOKEN, ..Default::default() }).unwrap();
    let mut data = SWEEP_FEES.to_vec();
    data.extend_from_slice(bytemuck::bytes_of(&SweepFeesArgs { which: 1, _padding: [0; 7] }));
    let claim = Instruction {
        program_id: w.program,
        accounts: vec![
            AccountMeta::new(market, false),
            AccountMeta::new(Pubkey::new_from_array(settled.vault.to_bytes()), false),
            AccountMeta::new(creator_ata, false),
            AccountMeta::new_readonly(w.quote_mint, false),
            AccountMeta::new_readonly(w.crank.deployment.config, false),
            AccountMeta::new_readonly(TOKEN, false),
            // The creator's sweep needs no operator: any signer will do, attested or not.
            AccountMeta::new_readonly(creator_ata, false),
            AccountMeta::new_readonly(creator_ata, false),
        ],
        data,
    };
    let claimer = Keypair::new();
    w.chain.svm.lock().unwrap().airdrop(&claimer.pubkey(), 1_000_000_000).unwrap();
    let mut claim = claim;
    claim.accounts[6] = AccountMeta::new_readonly(claimer.pubkey(), true);
    w.chain.send(&claimer, &[claim]).await.unwrap();
    assert_eq!(w.token_balance(&creator_ata), creator_owed);

    // ...and now the crank closes it, getting the rent back for its trouble.
    let reports = w.pass().await;
    assert_eq!(actions(&reports), vec![Action::CloseMarket]);
    all_ok(&reports);
    assert!(!w.exists(&market), "the market is gone");
    assert!(reports[0].spent.is_some_and(|s| s < 0), "closing returns more rent than it costs: {:?}", reports[0].spent);
    assert!(w.pass().await.is_empty(), "and there is nothing left to do");
}

// ── Cancelling ───────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn a_voided_result_is_cancelled_straight_away() {
    let Some(w) = World::new() else { return };
    let (market, oracle) = w.add_market(1, MarketStatus::Open, |_| {});
    w.edit_oracle(&oracle, |o| o.voided = 1);
    w.set_time(T0 + 60);
    let reports = w.pass().await;
    assert_eq!(actions(&reports), vec![Action::Cancel]);
    all_ok(&reports);
    let m = w.market(&market);
    assert_eq!(m.status, MarketStatus::Cancelled as u8);
    assert_eq!(m.payout_pool, 30_150_000, "everyone is refunded from the whole vault, fees included");
}

#[tokio::test(flavor = "current_thread")]
async fn no_result_by_the_deadline_cancels_and_not_a_second_before() {
    let Some(w) = World::new() else { return };
    let (market, _oracle) = w.add_market(1, MarketStatus::Closed, |_| {});
    w.set_time(KICKOFF + CANCEL_AFTER_SECS - 1);
    assert!(w.pass().await.is_empty());
    w.set_time(KICKOFF + CANCEL_AFTER_SECS);
    let reports = w.pass().await;
    assert_eq!(actions(&reports), vec![Action::Cancel]);
    all_ok(&reports);
    assert_eq!(w.market(&market).status, MarketStatus::Cancelled as u8);
}

#[tokio::test(flavor = "current_thread")]
async fn a_market_whose_oracle_account_is_gone_is_cancelled_once_overdue() {
    let Some(w) = World::new() else { return };
    let (market, oracle) = w.add_market(1, MarketStatus::Closed, |_| {});
    w.chain.svm.lock().unwrap().set_account(oracle, Account::default()).unwrap(); // closed away
    w.set_time(KICKOFF + CANCEL_AFTER_SECS);
    let reports = w.pass().await;
    assert_eq!(actions(&reports), vec![Action::Cancel]);
    all_ok(&reports);
    assert_eq!(w.market(&market).status, MarketStatus::Cancelled as u8);
}

#[tokio::test(flavor = "current_thread")]
async fn a_final_result_is_settled_never_cancelled_however_late() {
    let Some(w) = World::new() else { return };
    let (market, oracle) = w.add_market(1, MarketStatus::Closed, |_| {});
    let at = KICKOFF + 7_000;
    w.finalize(&oracle, 1, 1, at);
    w.set_time(KICKOFF + 20 * CANCEL_AFTER_SECS);
    let reports = w.pass().await;
    assert_eq!(actions(&reports), vec![Action::Settle]);
    all_ok(&reports);
    let m = w.market(&market);
    assert_eq!((m.status, m.winning_outcome), (MarketStatus::Settled as u8, 1), "a draw is NO on a home-win market");
}

// ── Cancelled markets close too ──────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn a_cancelled_market_is_closed_after_the_claim_window() {
    let Some(w) = World::new() else { return };
    let (market, oracle) = w.add_market(1, MarketStatus::Open, |_| {});
    w.edit_oracle(&oracle, |o| o.voided = 1);
    w.set_time(T0 + 60);
    all_ok(&w.pass().await);
    let resolved = w.market(&market).resolved_at;

    w.set_time(resolved + CLAIM_WINDOW_SECS - 1);
    assert!(w.pass().await.is_empty());
    w.set_time(resolved + CLAIM_WINDOW_SECS);
    let reports = w.pass().await;
    assert_eq!(actions(&reports), vec![Action::CloseMarket]);
    all_ok(&reports);
    assert!(!w.exists(&market));
}

// ── Robustness ───────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn one_markets_trouble_never_stops_the_others() {
    let Some(w) = World::new() else { return };
    // A healthy market whose result is final and past its window.
    let (good, good_oracle) = w.add_market(1, MarketStatus::Closed, |_| {});
    let at = KICKOFF + 7_000;
    w.finalize(&good_oracle, 2, 0, at);
    // One whose vault is short, so settling it fails on chain.
    let (short, short_oracle) = w.add_market(2, MarketStatus::Closed, |_| {});
    w.finalize(&short_oracle, 2, 0, at);
    let short_vault = Pubkey::new_from_array(w.market(&short).vault.to_bytes());
    w.chain.svm.lock().unwrap().set_account(short_vault, Account { lamports: 10_000_000, data: pack_token(&w.quote_mint, &short, 1), owner: TOKEN, ..Default::default() }).unwrap();
    // One whose oracle account is not an oracle at all.
    let (_garbled, garbled_oracle) = w.add_market(3, MarketStatus::Closed, |_| {});
    w.chain.svm.lock().unwrap().set_account(garbled_oracle, Account { lamports: 1_000_000, data: vec![1, 2, 3], ..Default::default() }).unwrap();

    w.set_time(at + DISPUTE);
    let reports = w.pass().await;

    let by_market = |a: &Pubkey| reports.iter().find(|r| r.market == *a);
    assert!(by_market(&good).unwrap().outcome.is_ok(), "the healthy market was settled");
    assert_eq!(w.market(&good).status, MarketStatus::Settled as u8);
    assert!(by_market(&short).unwrap().outcome.is_err(), "the short one failed, and was reported as such");
    assert_eq!(w.market(&short).status, MarketStatus::Closed as u8, "and was left untouched");
    assert!(reports.iter().all(|r| r.market != _garbled), "a market with a garbled oracle is left alone, not guessed at");
}

#[tokio::test(flavor = "current_thread")]
async fn a_pass_without_a_config_account_is_an_error_not_a_guess() {
    let Some(w) = World::new() else { return };
    w.add_market(1, MarketStatus::Open, |_| {});
    w.chain.svm.lock().unwrap().set_account(w.crank.deployment.config, Account::default()).unwrap();
    assert!(w.crank.run_once().await.is_err());
}

// ── What it costs ────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn each_step_reports_what_it_really_cost_the_payer() {
    let Some(w) = World::new() else { return };
    let (market, oracle) = w.add_market(1, MarketStatus::Open, |_| {});

    w.set_time(PRIMARY_END);
    let books = w.pass().await;
    let spent_books = books[0].spent.expect("measured");
    // Two order books and four vault accounts' rent, plus a fee: real money, and the thing a fee
    // layer would have to cover. Printed so a change that makes it dearer is visible.
    println!("close_primary cost {spent_books} lamports ({:.4} SOL)", spent_books as f64 / 1e9);
    assert!((5_000_000..40_000_000).contains(&spent_books), "{spent_books}");

    let at = KICKOFF + 7_000;
    w.finalize(&oracle, 2, 0, at);
    w.set_time(at + DISPUTE);
    let settle = w.pass().await;
    let spent_settle = settle[0].spent.unwrap();
    println!("settle_market cost {spent_settle} lamports");
    assert!((0..100_000).contains(&spent_settle), "settling is just a transaction fee: {spent_settle}");

    let sweep = w.pass().await;
    let spent_sweep = sweep[0].spent.unwrap();
    println!("sweep_fees cost {spent_sweep} lamports (includes the treasury's token account the first time)");
    assert!(spent_sweep > spent_settle, "the first sweep also pays for the treasury's token account");
    let _ = market;
}

// ── Gating ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn a_key_that_is_not_an_attested_operator_changes_nothing() {
    let Some(w) = World::new() else { return };
    let (market, oracle) = w.add_market(1, MarketStatus::Open, |_| {});
    // Attestation never completed (one root only): as good as none.
    {
        let mut svm = w.chain.svm.lock().unwrap();
        write_attestation(&mut svm, &w.program, &w.crank.payer.pubkey(), false);
    }
    w.set_time(PRIMARY_END);
    let reports = w.pass().await;
    assert_eq!(actions(&reports), vec![Action::ClosePrimary]);
    assert!(reports[0].outcome.is_err(), "refused on chain");
    assert_eq!(w.market(&market).status, MarketStatus::Open as u8, "and nothing changed");

    let at = KICKOFF + 7_000;
    w.finalize(&oracle, 2, 0, at);
    w.set_time(at + DISPUTE);
    let reports = w.pass().await;
    assert!(reports[0].outcome.is_err());
    assert_eq!(w.market(&market).status, MarketStatus::Open as u8);

    // The same key, once attested, goes straight through.
    {
        let mut svm = w.chain.svm.lock().unwrap();
        write_attestation(&mut svm, &w.program, &w.crank.payer.pubkey(), true);
    }
    all_ok(&w.pass().await);
    assert_eq!(w.market(&market).status, MarketStatus::Settled as u8);
}

#[tokio::test(flavor = "current_thread")]
async fn the_crank_only_lists_forfeitable_fees_and_never_forfeits_them() {
    let Some(w) = World::new() else { return };
    let (market, _) = w.add_market(1, MarketStatus::Settled, |m| {
        m.creator_owed = 1_000;
        m.resolved_at = T0;
    });
    let m = w.market(&market);
    let late = T0 + metis_core::constants::CREATOR_CLAIM_SECS;
    assert!(!oracle_keeper::crank::forfeitable(&m, late - 1));
    assert!(oracle_keeper::crank::forfeitable(&m, late));
    // Long past the deadline, a pass still leaves the creator's fees alone.
    w.set_time(late + 1_000_000);
    for _ in 0..3 {
        w.pass().await;
    }
    assert_eq!(w.market(&market).creator_owed, 1_000);
    assert_eq!(w.market(&market).forfeited_creator_fees, 0);
}
