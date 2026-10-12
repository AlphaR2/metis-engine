//! Real keepers against the real oracle program (the compiled .so, in litesvm), reading from
//! scripted providers. This is the whole pipeline except the HTTP calls to a real provider.
//!
//! Needs the program built first: `cargo build-sbf --manifest-path programs/oracle/Cargo.toml`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use litesvm::LiteSVM;
use oracle::{
    constants::{INDIVIDUAL_AUTHORITY_DISCRIMINATOR, ORACLE_AUTHORITY_DISCRIMINATOR, ROOT_SIGNERS, GUARDIAN},
    state::{DelegatedAuthority, RootAuthority},
};
use oracle_keeper::{
    chain::{self, Chain, ChainError},
    fixture::fixture_root,
    keeper::{Keeper, Tick, Watch},
    model::{FootballSettled, Observation, Phase},
    sources::scripted::Scripted,
};
use solana_account::Account;
use solana_clock::Clock;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

const T0: i64 = 1_000_000;
const KICKOFF: i64 = T0 + 6 * 60 * 60;

/// The oracle program inside litesvm, shared by every keeper in a test.
#[derive(Clone)]
struct LocalChain(Arc<Mutex<LiteSVM>>);

impl LocalChain {
    fn new() -> Self {
        let mut svm = LiteSVM::new();
        let mut clock = svm.get_sysvar::<Clock>();
        clock.unix_timestamp = T0;
        svm.set_sysvar(&clock);
        let so = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/oracle.so"))
            .expect("build the oracle first: cargo build-sbf --manifest-path programs/oracle/Cargo.toml");
        svm.add_program(chain::oracle_program(), &so).unwrap();
        svm.set_account(
            chain::authority_pda(),
            Account {
                lamports: 1_000_000,
                data: bytemuck::bytes_of(&RootAuthority::new(&ROOT_SIGNERS, GUARDIAN, ORACLE_AUTHORITY_DISCRIMINATOR)).to_vec(),
                owner: chain::oracle_program(),
                ..Default::default()
            },
        )
        .unwrap();
        Self(Arc::new(Mutex::new(svm)))
    }

    /// A funded keypair that two roots have attested, so it may report.
    fn reporter(&self, seed: u8) -> Keypair {
        let kp = Keypair::new_from_array([seed; 32]);
        let mut svm = self.0.lock().unwrap();
        svm.airdrop(&kp.pubkey(), 10_000_000_000).unwrap();
        let mut attested = DelegatedAuthority::new(
            solana_address::Address::from(kp.pubkey().to_bytes()),
            INDIVIDUAL_AUTHORITY_DISCRIMINATOR,
        );
        attested.attesters = [ROOT_SIGNERS[0].to_bytes(), ROOT_SIGNERS[1].to_bytes()];
        attested.is_attest_complete = 1u64.to_le_bytes();
        svm.set_account(
            chain::individual_authority_pda(&kp.pubkey()),
            Account {
                lamports: 1_000_000,
                data: bytemuck::bytes_of(&attested).to_vec(),
                owner: chain::oracle_program(),
                ..Default::default()
            },
        )
        .unwrap();
        kp
    }

    fn set_time(&self, ts: i64) {
        let mut svm = self.0.lock().unwrap();
        let mut clock = svm.get_sysvar::<Clock>();
        clock.unix_timestamp = ts;
        svm.set_sysvar(&clock);
    }

    /// Edits the oracle account directly, to start a scenario partway through a match.
    fn edit_state(&self, root: &[u8; 32], edit: impl FnOnce(&mut FootballSettled)) {
        let mut svm = self.0.lock().unwrap();
        let address = chain::oracle_pda(root);
        let mut account = svm.get_account(&address).expect("oracle exists");
        let mut s = *bytemuck::checked::try_from_bytes::<FootballSettled>(&account.data).unwrap();
        edit(&mut s);
        account.data = s.as_bytes().to_vec();
        svm.set_account(address, account).unwrap();
    }

    fn state(&self, root: &[u8; 32]) -> FootballSettled {
        let svm = self.0.lock().unwrap();
        let data = svm.get_account(&chain::oracle_pda(root)).expect("oracle exists").data;
        *bytemuck::checked::try_from_bytes::<FootballSettled>(&data).unwrap()
    }
}

#[async_trait]
impl Chain for LocalChain {
    async fn oracle(&self, address: &Pubkey) -> Result<Option<FootballSettled>, ChainError> {
        let svm = self.0.lock().unwrap();
        svm.get_account(address).map(|a| chain::decode_oracle(address, &a.data)).transpose()
    }

    async fn send(&self, payer: &Keypair, ixs: &[Instruction]) -> Result<String, ChainError> {
        let mut svm = self.0.lock().unwrap();
        // A fresh blockhash each time, or identical transactions are refused as already processed.
        svm.expire_blockhash();
        let blockhash = svm.latest_blockhash();
        let message = Message::new_with_blockhash(ixs, Some(&payer.pubkey()), &blockhash);
        let tx = Transaction::new(&[payer], message, blockhash);
        svm.send_transaction(tx)
            .map(|meta| meta.signature.to_string())
            .map_err(|e| ChainError::Rpc(format!("{:?}", e.err)))
    }
}

fn obs(phase: Phase, score: (u8, u8)) -> Observation {
    Observation {
        source: "scripted",
        phase,
        score,
        regulation: None,
        penalties: None,
        went_to_extra_time: false,
        went_to_penalties: false,
    }
}

/// A 90-minute match that ends 2-1.
fn ninety_minutes() -> Vec<Observation> {
    let mut finished = obs(Phase::Finished, (2, 1));
    finished.regulation = Some((2, 1));
    vec![
        obs(Phase::Scheduled, (0, 0)),
        obs(Phase::FirstHalf, (0, 0)),
        obs(Phase::HalfTime, (1, 0)),
        obs(Phase::SecondHalf, (2, 1)),
        finished,
    ]
}

/// 1-1 after 90, 3-1 after extra time.
fn extra_time() -> Vec<Observation> {
    let mut et = obs(Phase::ExtraTimeSecondHalf, (3, 1));
    et.regulation = Some((1, 1));
    et.went_to_extra_time = true;
    let mut finished = et.clone();
    finished.phase = Phase::Finished;
    vec![
        obs(Phase::Scheduled, (0, 0)),
        obs(Phase::FirstHalf, (0, 1)),
        obs(Phase::SecondHalf, (1, 1)),
        et,
        finished,
    ]
}

struct Rig {
    chain: LocalChain,
    watch_for: Vec<Watch>,
    root: [u8; 32],
}

fn rig(providers: &[(&str, &str)]) -> Rig {
    let chain = LocalChain::new();
    let root = fixture_root(providers);
    let watch_for = providers
        .iter()
        .map(|(_, id)| Watch { fixture_root: root, external_id: id.to_string() })
        .collect();
    Rig { chain, watch_for, root }
}

fn keeper(chain: &LocalChain, seed: u8, source: &Scripted) -> Keeper<LocalChain> {
    Keeper::new(chain.clone(), chain.reporter(seed), Arc::new(source.clone()))
}

async fn create_fixture(r: &Rig, creator: &Keeper<LocalChain>) {
    let ix = chain::create_ix(&creator.reporter.pubkey(), creator.attestation, &r.root, KICKOFF);
    r.chain.send(&creator.reporter, &[ix]).await.expect("fixture is created");
}

/// Ticks every keeper until none of them has anything left to send.
async fn settle_down(keepers: &[&Keeper<LocalChain>], r: &Rig) -> Vec<String> {
    let mut log = Vec::new();
    for _ in 0..40 {
        let mut sent = false;
        for (i, k) in keepers.iter().enumerate() {
            match k.tick(&r.watch_for[i % r.watch_for.len()]).await {
                Ok(Tick::Submitted { claim, .. }) => {
                    sent = true;
                    log.push(format!("keeper {i}: {:?} {:#04x}", claim.status, claim.score));
                }
                Ok(_) => {}
                Err(e) => log.push(format!("keeper {i} error: {e}")),
            }
        }
        if !sent {
            break;
        }
    }
    log
}

fn step_all(sources: &[&Scripted]) {
    for s in sources {
        s.advance();
    }
}

#[tokio::test]
async fn two_providers_walk_a_match_to_a_final_result() {
    let r = rig(&[("provider-a", "1"), ("provider-b", "2")]);
    let (a, b) = (Scripted::new("provider-a", ninety_minutes()), Scripted::new("provider-b", ninety_minutes()));
    let (ka, kb) = (keeper(&r.chain, 1, &a), keeper(&r.chain, 2, &b));
    create_fixture(&r, &ka).await;

    for _ in 0..5 {
        r.chain.set_time(KICKOFF + 60);
        settle_down(&[&ka, &kb], &r).await;
        step_all(&[&a, &b]);
    }
    // Both providers have now shown the final whistle.
    let log = settle_down(&[&ka, &kb], &r).await;
    println!("{log:#?}");

    let s = r.chain.state(&r.root);
    assert!(s.is_finalized(), "the result is final");
    assert_eq!(s.get_score(), (2, 1));
    assert_eq!(s.regulation(), (2, 1));
}

#[tokio::test]
async fn extra_time_does_not_leak_into_the_ninety_minute_score() {
    let r = rig(&[("provider-a", "1"), ("provider-b", "2")]);
    let (a, b) = (Scripted::new("provider-a", extra_time()), Scripted::new("provider-b", extra_time()));
    let (ka, kb) = (keeper(&r.chain, 1, &a), keeper(&r.chain, 2, &b));
    create_fixture(&r, &ka).await;

    for _ in 0..5 {
        r.chain.set_time(KICKOFF + 60);
        settle_down(&[&ka, &kb], &r).await;
        step_all(&[&a, &b]);
    }
    settle_down(&[&ka, &kb], &r).await;

    let s = r.chain.state(&r.root);
    assert!(s.is_finalized());
    assert_eq!(s.get_score(), (3, 1), "the final score includes extra time");
    assert_eq!(s.regulation(), (1, 1), "the 90-minute score does not");
}

#[tokio::test]
async fn a_provider_that_disagrees_cannot_finalise_and_a_third_breaks_the_tie() {
    let r = rig(&[("provider-a", "1"), ("provider-b", "2"), ("provider-c", "3")]);
    let honest = ninety_minutes();
    let mut lying = ninety_minutes();
    let last = lying.len() - 1;
    lying[last].score = (2, 2);
    lying[last].regulation = Some((2, 2));
    let (a, b, c) = (
        Scripted::new("provider-a", honest.clone()),
        Scripted::new("provider-b", lying),
        Scripted::new("provider-c", honest),
    );
    let (ka, kb) = (keeper(&r.chain, 1, &a), keeper(&r.chain, 2, &b));
    create_fixture(&r, &ka).await;

    r.chain.set_time(KICKOFF + 60);
    for _ in 0..5 {
        settle_down(&[&ka, &kb], &r).await;
        step_all(&[&a, &b]);
    }
    settle_down(&[&ka, &kb], &r).await;
    let s = r.chain.state(&r.root);
    assert!(!s.is_finalized(), "two disagreeing providers must not finalise");
    assert_eq!(s.status, Phase::SecondHalf);

    // A third provider comes online, sees the honest final, and sides with A.
    c.advance();
    c.advance();
    c.advance();
    c.advance();
    let kc = keeper(&r.chain, 3, &c);
    for _ in 0..10 {
        let _ = kc.tick(&r.watch_for[2]).await.unwrap();
    }
    let s = r.chain.state(&r.root);
    assert!(s.is_finalized());
    assert_eq!(s.get_score(), (2, 1), "the two honest providers win");
}

#[tokio::test]
async fn a_keeper_that_was_offline_catches_up_without_help() {
    let r = rig(&[("provider-a", "1"), ("provider-b", "2")]);
    let (a, b) = (Scripted::new("provider-a", ninety_minutes()), Scripted::new("provider-b", ninety_minutes()));
    let (ka, kb) = (keeper(&r.chain, 1, &a), keeper(&r.chain, 2, &b));
    create_fixture(&r, &ka).await;
    r.chain.set_time(KICKOFF + 60);

    // Only A is running through the first half of the match: it alone moves nothing.
    for _ in 0..3 {
        a.advance();
        let _ = ka.tick(&r.watch_for[0]).await.unwrap();
    }
    assert_eq!(r.chain.state(&r.root).status, Phase::Scheduled, "one keeper alone changes nothing");

    // B comes online late; its provider already shows the end. It walks the phases it missed
    // and the pair converge on the final result.
    for _ in 0..4 {
        b.advance();
    }
    a.advance();
    settle_down(&[&ka, &kb], &r).await;
    let s = r.chain.state(&r.root);
    assert!(s.is_finalized());
    assert_eq!(s.get_score(), (2, 1));
}

#[tokio::test]
async fn a_keeper_waits_when_its_provider_withholds_the_ninety_minute_score() {
    let r = rig(&[("provider-a", "1")]);
    let mut withheld = extra_time();
    for o in &mut withheld {
        o.regulation = None;
    }
    let a = Scripted::new("provider-a", withheld);
    let ka = keeper(&r.chain, 1, &a);
    create_fixture(&r, &ka).await;
    r.chain.set_time(KICKOFF + 60);

    // The match is in the second half on chain (two providers got it there), and A's provider now
    // shows extra time but never says what the score was at 90 minutes.
    r.chain.edit_state(&r.root, |s| {
        s.status = Phase::SecondHalf;
        s.score = FootballSettled::pack_score(1, 1);
    });
    for _ in 0..4 {
        a.advance();
    }
    let mut waiting = false;
    for _ in 0..3 {
        if let Tick::Waiting(why) = ka.tick(&r.watch_for[0]).await.unwrap() {
            waiting = true;
            println!("waiting: {why}");
            break;
        }
    }
    assert!(waiting, "it must hold back rather than guess the 90-minute score");
    assert!(!r.chain.state(&r.root).is_finalized());
}

#[tokio::test]
async fn ticks_report_when_there_is_nothing_to_do() {
    let r = rig(&[("provider-a", "1"), ("provider-b", "2")]);
    let a = Scripted::new("provider-a", ninety_minutes());
    let ka = keeper(&r.chain, 1, &a);

    assert!(matches!(ka.tick(&r.watch_for[0]).await.unwrap(), Tick::NotCreated));
    create_fixture(&r, &ka).await;
    assert!(matches!(ka.tick(&r.watch_for[0]).await.unwrap(), Tick::UpToDate), "scheduled and quiet");

    let b = Scripted::new("provider-b", ninety_minutes());
    let kb = keeper(&r.chain, 2, &b);
    r.chain.set_time(KICKOFF + 60);
    for _ in 0..5 {
        settle_down(&[&ka, &kb], &r).await;
        step_all(&[&a, &b]);
    }
    settle_down(&[&ka, &kb], &r).await;
    assert!(matches!(ka.tick(&r.watch_for[0]).await.unwrap(), Tick::Settled));
}

#[tokio::test]
async fn an_unattested_key_cannot_report() {
    let r = rig(&[("provider-a", "1")]);
    let a = Scripted::new("provider-a", ninety_minutes());
    let ka = keeper(&r.chain, 1, &a);
    create_fixture(&r, &ka).await;
    r.chain.set_time(KICKOFF + 60);

    // A key nobody attested: funded, but with no attestation account behind it.
    let stranger = Keypair::new_from_array([99; 32]);
    r.chain.0.lock().unwrap().airdrop(&stranger.pubkey(), 10_000_000_000).unwrap();
    let ix = chain::submit_ix(
        &stranger.pubkey(),
        Some(chain::individual_authority_pda(&stranger.pubkey())),
        &r.root,
        &oracle_keeper::model::Claim { status: Phase::FirstHalf, score: 0, penalties: 0 },
    );
    assert!(r.chain.send(&stranger, &[ix]).await.is_err());
}
