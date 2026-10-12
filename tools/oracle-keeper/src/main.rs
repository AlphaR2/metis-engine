//! oracle-keeper: reports match results to the Metis oracle.
//!
//!   oracle-keeper run
//!       Watches every match listed in KEEPER_WATCH and reports what its provider says.
//!   oracle-keeper create-fixture --kickoff <unix> --id <provider>=<external id> [--id ...]
//!       Creates the oracle account for a match. Prints the fixture root to put in the watch file.
//!   oracle-keeper show <fixture root hex>
//!       Prints what the oracle holds for a match.
//!   oracle-keeper crank [--once]
//!       Sends the transactions that move every market through its life: opening its order
//!       books, settling, cancelling, sweeping the protocol's fees, closing. The key must be an
//!       attested operator (two roots attested it): these steps are gated. It never pays the
//!       creator's fees (the creator claims those) and never forfeits them. `--once` does a
//!       single pass and exits, for cron.
//!   oracle-keeper forfeitable
//!       Lists markets whose creator fees have gone unclaimed past the claim period. Read only.
//!   oracle-keeper forfeit <market> --root-a <keypair file> --root-b <keypair file>
//!       Two roots move one market's unclaimed creator fees to the treasury (recorded on the market).
//!
//! Configuration is by environment variable:
//!   KEEPER_RPC_URL            Solana RPC endpoint (required)
//!   KEEPER_KEYPAIR            path to the reporter's keypair file (a JSON array of 64 bytes)
//!   KEEPER_PROVIDER           football-data | api-football (run only)
//!   PROVIDER_API_KEY          the provider's key (run only)
//!   KEEPER_PROVIDER_BASE_URL  optional; point the provider at another host, e.g. a local mock
//!   KEEPER_WATCH              path to a JSON file: [{"fixture_root": "<hex>", "external_id": "<id>"}]
//!   KEEPER_POLL_SECS          seconds between rounds (default 30)
//!   KEEPER_HEALTH_FILE        optional; written after every round with `<unix time> ok|degraded`
//!   RPC calls are retried with backoff; Ctrl-C / SIGTERM stops cleanly between rounds.
//!   CRANK_PROGRAM_ID          the deployed market program (default: this build's program id)
//!   CRANK_CONFIG              its Config account (default: derived from this build's trust roots -
//!                             build with `--features devnet` / `mainnet` to match a deployment)

use std::{sync::Arc, time::Duration};

use oracle_keeper::{
    chain::{self, Chain, RpcChain},
    crank::{Crank, Deployment},
    fixture::{self, fixture_root},
    keeper::{Keeper, Tick, Watch},
    sources::{ScoreSource, api_football::ApiFootball, football_data::FootballData},
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set"))
}

fn load_keypair() -> Result<Keypair, String> {
    let path = env("KEEPER_KEYPAIR")?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?;
    let bytes: Vec<u8> = serde_json::from_str(&text).map_err(|e| format!("{path} is not a keypair file: {e}"))?;
    Keypair::try_from(bytes.as_slice()).map_err(|e| format!("{path} is not a valid keypair: {e}"))
}

fn build_source() -> Result<Arc<dyn ScoreSource>, String> {
    let key = env("PROVIDER_API_KEY")?;
    let base = std::env::var("KEEPER_PROVIDER_BASE_URL").ok();
    match env("KEEPER_PROVIDER")?.as_str() {
        "football-data" => Ok(Arc::new(match base {
            Some(b) => FootballData::with_base_url(b, key),
            None => FootballData::new(key),
        })),
        "api-football" => Ok(Arc::new(match base {
            Some(b) => ApiFootball::with_base_url(b, key),
            None => ApiFootball::new(key),
        })),
        other => Err(format!("unknown provider '{other}' (football-data | api-football)")),
    }
}

#[derive(serde::Deserialize)]
struct WatchEntry {
    fixture_root: String,
    external_id: String,
}

fn load_watch() -> Result<Vec<Watch>, String> {
    let path = env("KEEPER_WATCH")?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?;
    let entries: Vec<WatchEntry> = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    entries
        .into_iter()
        .map(|e| {
            let root = fixture::from_hex(&e.fixture_root)
                .ok_or_else(|| format!("{} is not a 64-character hex fixture root", e.fixture_root))?;
            Ok(Watch { fixture_root: root, external_id: e.external_id })
        })
        .collect()
}

async fn run() -> Result<(), String> {
    let reporter = load_keypair()?;
    let source = build_source()?;
    let watches = load_watch()?;
    let poll = Duration::from_secs(std::env::var("KEEPER_POLL_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(30));
    let keeper = Keeper::new(chain::Retrying::new(RpcChain::new(&env("KEEPER_RPC_URL")?)), reporter, source);

    tracing::info!(reporter = %keeper.reporter.pubkey(), provider = keeper.source.id(), matches = watches.len(), "keeper started");
    loop {
        let mut healthy = true;
        for watch in &watches {
            let root = fixture::to_hex(&watch.fixture_root);
            match keeper.tick(watch).await {
                Ok(Tick::Submitted { claim, signature }) => {
                    tracing::info!(%root, status = ?claim.status, score = claim.score, %signature, "reported")
                }
                Ok(Tick::Waiting(why)) => tracing::warn!(%root, why, "holding back"),
                Ok(Tick::SourceUnavailable(why)) => {
                    healthy = false;
                    tracing::warn!(%root, why, "provider unavailable")
                }
                Ok(Tick::NotCreated) => tracing::debug!(%root, "oracle not created yet"),
                Ok(Tick::UpToDate | Tick::Settled) => tracing::debug!(%root, "nothing to do"),
                Err(e) => {
                    healthy = false;
                    tracing::error!(%root, error = %e, "tick failed")
                }
            }
        }
        heartbeat(healthy);
        if stop_requested_within(poll).await {
            return Ok(());
        }
    }
}

/// Sleeps for `poll`, or returns true at once if the process was asked to stop (Ctrl-C / SIGTERM),
/// so a deploy or a restart never cuts a transaction in half.
async fn stop_requested_within(poll: Duration) -> bool {
    #[cfg(unix)]
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    #[cfg(unix)]
    let term_signal = async {
        match term.as_mut() {
            Some(s) => {
                s.recv().await;
            }
            None => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term_signal = std::future::pending::<()>();
    tokio::select! {
        _ = tokio::time::sleep(poll) => false,
        _ = tokio::signal::ctrl_c() => { tracing::info!("stop requested; shutting down"); true }
        _ = term_signal => { tracing::info!("stop requested; shutting down"); true }
    }
}

/// Writes `<unix time> ok|degraded` to KEEPER_HEALTH_FILE after every round, if set, so a process
/// supervisor (systemd, Kubernetes, cron) can tell a live loop from a wedged one by the file's age
/// and a working one from a failing one by its word.
fn heartbeat(healthy: bool) {
    let Ok(path) = std::env::var("KEEPER_HEALTH_FILE") else { return };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if let Err(e) = std::fs::write(&path, format!("{now} {}\n", if healthy { "ok" } else { "degraded" })) {
        tracing::warn!(error = %e, %path, "could not write the health file");
    }
}

fn deployment() -> Result<Deployment, String> {
    let parse = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|text| text.trim().parse::<Pubkey>().map_err(|_| format!("{name} '{text}' is not a valid address")))
            .transpose()
    };
    let mut d = Deployment::for_this_build();
    if let Some(program) = parse("CRANK_PROGRAM_ID")? {
        d.program = program;
        d.config = Deployment::default_config(&program);
    }
    if let Some(config) = parse("CRANK_CONFIG")? {
        d.config = config;
    }
    Ok(d)
}

async fn crank(args: &[String]) -> Result<(), String> {
    let once = args.iter().any(|a| a == "--once");
    if let Some(other) = args.iter().find(|a| *a != "--once") {
        return Err(format!("unknown argument {other}"));
    }
    let payer = load_keypair()?;
    let poll = Duration::from_secs(std::env::var("KEEPER_POLL_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(30));
    let crank = Crank::new(chain::Retrying::new(RpcChain::new(&env("KEEPER_RPC_URL")?)), payer, deployment()?);
    tracing::info!(payer = %crank.payer.pubkey(), program = %crank.deployment.program, "crank started");

    loop {
        let healthy = match crank.run_once().await {
            Ok(reports) => {
                let mut healthy = true;
                for r in reports {
                    let spent = r.spent.map_or("?".to_string(), |s| format!("{s} lamports"));
                    match r.outcome {
                        Ok(signature) => tracing::info!(market = %r.market, action = ?r.action, %signature, %spent, "done"),
                        Err(why) => {
                            healthy = false;
                            tracing::warn!(market = %r.market, action = ?r.action, %why, "did not go through; will try again next pass")
                        }
                    }
                }
                healthy
            }
            Err(e) => {
                tracing::error!(error = %e, "pass failed");
                false
            }
        };
        heartbeat(healthy);
        if once || stop_requested_within(poll).await {
            return Ok(());
        }
    }
}

/// Lists markets whose creator fees have sat unclaimed past the claim period. Read only: it is the
/// roots who decide whether to forfeit them, with `forfeit`.
async fn forfeitable() -> Result<(), String> {
    let chain = RpcChain::new(&env("KEEPER_RPC_URL")?);
    let deployment = deployment()?;
    let now = chain.now().await.map_err(|e| e.to_string())?;
    let rows = chain
        .program_accounts(
            &deployment.program,
            core::mem::size_of::<metis_core::amm::state::Market>(),
            core::mem::offset_of!(metis_core::amm::state::Market, discriminator),
            &metis_core::amm::state::MARKET_DISCRIMINATOR,
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut n = 0;
    for (address, data) in rows {
        let Some(m) = oracle_keeper::crank::decode_market(&data) else { continue };
        if oracle_keeper::crank::forfeitable(&m, now) {
            n += 1;
            println!("{address}  creator {}  unclaimed {} atoms  settled at {}", m.creator, m.creator_owed, m.resolved_at);
        }
    }
    println!("{n} market(s) forfeitable");
    Ok(())
}

/// `forfeit <market> --root-a <keypair file> --root-b <keypair file>`: two distinct roots move a
/// market's unclaimed creator fees to the treasury. The record stays on the market.
async fn forfeit(args: &[String]) -> Result<(), String> {
    let usage = "usage: oracle-keeper forfeit <market> --root-a <keypair file> --root-b <keypair file>";
    let market_address: Pubkey = args.first().and_then(|a| a.parse().ok()).ok_or(usage)?;
    let mut a = None;
    let mut b = None;
    let mut it = args[1..].iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--root-a" => a = it.next().cloned(),
            "--root-b" => b = it.next().cloned(),
            other => return Err(format!("unknown argument {other}. {usage}")),
        }
    }
    let read = |p: Option<String>| -> Result<Keypair, String> {
        let p = p.ok_or(usage)?;
        solana_keypair::read_keypair_file(&p).map_err(|e| format!("{p}: {e}"))
    };
    let (root_a, root_b) = (read(a)?, read(b)?);

    let chain = RpcChain::new(&env("KEEPER_RPC_URL")?);
    let deployment = deployment()?;
    let data = chain.account(&market_address).await.map_err(|e| e.to_string())?.ok_or("no such market account")?;
    let market = oracle_keeper::crank::decode_market(&data).ok_or("that account is not a market")?;
    let now = chain.now().await.map_err(|e| e.to_string())?;
    if !oracle_keeper::crank::forfeitable(&market, now) {
        return Err("nothing to forfeit yet: the market is not settled, has no unclaimed creator fees, or the claim period is not over".into());
    }
    let config = chain.account(&deployment.config).await.map_err(|e| e.to_string())?.ok_or("no config account")?;
    let treasury = oracle_keeper::crank::treasury_of(&config).ok_or("config account unreadable")?;
    let ix = oracle_keeper::crank::forfeit_instruction(&deployment, &market_address, &market, &treasury, &root_a.pubkey(), &root_b.pubkey());
    let signature = chain.send_signed(&[&root_a, &root_b], &[ix]).await.map_err(|e| e.to_string())?;
    println!("forfeited {} atoms of {}'s fees to the treasury: {signature}", market.creator_owed, market.creator);
    Ok(())
}

async fn create_fixture(args: &[String]) -> Result<(), String> {
    let mut kickoff: Option<i64> = None;
    let mut ids: Vec<(String, String)> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--kickoff" => kickoff = it.next().and_then(|v| v.parse().ok()),
            "--id" => {
                let v = it.next().ok_or("--id needs provider=external_id")?;
                let (p, e) = v.split_once('=').ok_or("--id needs provider=external_id")?;
                ids.push((p.to_string(), e.to_string()));
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let kickoff = kickoff.ok_or("--kickoff <unix seconds> is required")?;
    if ids.is_empty() {
        return Err("at least one --id provider=external_id is required".into());
    }

    let signer = load_keypair()?;
    let pairs: Vec<(&str, &str)> = ids.iter().map(|(p, e)| (p.as_str(), e.as_str())).collect();
    let root = fixture_root(&pairs);
    let chain = RpcChain::new(&env("KEEPER_RPC_URL")?);
    let attestation: Pubkey = chain::individual_authority_pda(&signer.pubkey());
    let ix = chain::create_ix(&signer.pubkey(), Some(attestation), &root, kickoff);
    let signature = chain.send(&signer, &[ix]).await.map_err(|e| e.to_string())?;

    println!("fixture_root: {}", fixture::to_hex(&root));
    println!("oracle:       {}", chain::oracle_pda(&root));
    println!("signature:    {signature}");
    Ok(())
}

async fn show(args: &[String]) -> Result<(), String> {
    let root = args.first().and_then(|h| fixture::from_hex(h)).ok_or("usage: oracle-keeper show <64-char hex fixture root>")?;
    let chain = RpcChain::new(&env("KEEPER_RPC_URL")?);
    let address = chain::oracle_pda(&root);
    match chain.oracle(&address).await.map_err(|e| e.to_string())? {
        None => println!("{address}: no oracle account"),
        Some(s) => {
            println!("oracle:      {address}");
            println!("status:      {:?}", s.status);
            println!("score:       {:?}   regulation: {:?}", s.get_score(), s.regulation());
            println!("kickoff:     {}", s.kickoff());
            println!("finalized:   {}   voided: {}   overridden: {}", s.is_finalized(), s.is_voided(), s.overridden != 0);
            println!("reports:     {}", s.reports.iter().filter(|r| r.used != 0).count());
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => run().await,
        Some("create-fixture") => create_fixture(&args[1..]).await,
        Some("show") => show(&args[1..]).await,
        Some("crank") => crank(&args[1..]).await,
        Some("forfeitable") => forfeitable().await,
        Some("forfeit") => forfeit(&args[1..]).await,
        _ => Err("usage: oracle-keeper run | crank [--once] | forfeitable | forfeit <market> --root-a <file> --root-b <file> | create-fixture --kickoff <unix> --id <provider>=<id> | show <root>".into()),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
