//! metis-ops: setup and operator commands. Keypairs are files (a JSON array of 64 bytes).
//!
//!   metis-ops oracle-init   --root <kp>
//!   metis-ops attest        --program oracle|core --root <kp> --candidate <pubkey>
//!   metis-ops config-init   --root-a <kp> --root-b <kp> --mint <pubkey> --treasury <pubkey>
//!                           [--init-fee <atoms>] [--k <n>] [--dispute-secs <n>] [--fee-bps <n>]
//!                           [--rake-bps <n>] [--max-in-bps <n>] [--dbc 0|1]
//!   metis-ops create-market --creator <kp> --mint <pubkey> --oracle <pubkey> --nonce <n>
//!                           --condition home-win|away-win|draw|goals-over:<n>|both-score
//!                           --question <t> --rubric-yes <t> --rubric-no <t> --label-yes <t>
//!                           --label-no <t> [--category <t>] [--league <t>]
//!   metis-ops create-backed-market  (create-market's flags, plus) --backer-mint <pk> --seed <usdc>
//!                           --raise-secs <n> [--platform <pk>]   (opens a Pending, DBC-backed market)
//!   metis-ops activate      --operator <kp> --market <pk>        (after the raise funded the vault)
//!   metis-ops cancel-pending --signer <kp> --market <pk>
//!   metis-ops claim-backer  --backer <kp> --market <pk> --mint <pk>
//!   metis-ops bet           --bettor <kp> --market <pubkey> --mint <pubkey> --side 0|1 --usdc <n>
//!   metis-ops claim|refund  --holder <kp> --market <pk> --mint <pk> --side 0|1
//!   metis-ops creator-claim --creator <kp> --market <pk> --mint <pk>
//!   metis-ops forfeit       --root-a <kp> --root-b <kp> --market <pk> --mint <pk> --treasury <pk>
//!   metis-ops settle|cancel --signer <kp> --market <pk> --oracle <pk>
//!   metis-ops pause         --root <kp>
//!   metis-ops unpause       --guardian <kp> --root-a <kp> --root-b <kp>
//!   metis-ops show          --market <pk>
//!   metis-ops market-json   --market <pk>   (one line of JSON, for scripts)
//!   metis-ops pubkey        --keypair <kp>   (prints that keypair's public address; the secret is never printed)
//!   metis-ops addresses     (prints the program, config and authority addresses of this build)
//!
//! RPC_URL (default http://127.0.0.1:8899). Build with the same network feature as the deployed
//! programs (`--features localnet|devnet|mainnet`): the trust roots are compiled in.
//!
//! `config-init`'s dial defaults are for a local run. For a real network pass every dial.

use std::collections::HashMap;

use metis_core::{
    amm::condition::{
        AWAY_GOALS_OVER, AWAY_WIN, AWAY_WIN_BY_OVER, BOTH_TEAMS_SCORE, DRAW, HOME_GOALS_OVER, HOME_WIN,
        HOME_WIN_BY_OVER, TOTAL_GOALS_OVER,
    },
    config::state::ConfigDials,
    constants::{MAX_OUTCOMES, MAX_PRICE_IMPACT_BPS, MIN_DISPUTE_SECS, MIN_SEED, VIRTUAL_LIQUIDITY_MULTIPLE},
};
use metis_ops::*;
use solana_keypair::Keypair;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use solana_transaction::Transaction;

fn flags(args: &[String]) -> Result<HashMap<String, String>, String> {
    let mut out = HashMap::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let name = a.strip_prefix("--").ok_or(format!("unexpected argument {a}"))?;
        out.insert(name.to_string(), it.next().ok_or(format!("--{name} needs a value"))?.clone());
    }
    Ok(out)
}

struct Args(HashMap<String, String>);

impl Args {
    fn get(&self, k: &str) -> Result<&str, String> {
        self.0.get(k).map(String::as_str).ok_or(format!("--{k} is required"))
    }
    fn opt(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(String::as_str)
    }
    fn num<T: std::str::FromStr>(&self, k: &str, default: T) -> Result<T, String> {
        self.opt(k).map_or(Ok(default), |v| v.parse().map_err(|_| format!("--{k} '{v}' is not a number")))
    }
    fn key(&self, k: &str) -> Result<Pubkey, String> {
        self.get(k)?.parse().map_err(|_| format!("--{k} is not a valid address"))
    }
    fn keypair(&self, k: &str) -> Result<Keypair, String> {
        let p = self.get(k)?;
        solana_keypair::read_keypair_file(p).map_err(|e| format!("{p}: {e}"))
    }
}

async fn send(client: &RpcClient, signers: &[&Keypair], ixs: &[solana_instruction::Instruction]) -> Result<String, String> {
    let blockhash = client.get_latest_blockhash().await.map_err(|e| e.to_string())?;
    let message = Message::new_with_blockhash(ixs, Some(&signers[0].pubkey()), &blockhash);
    let tx = Transaction::new(signers, message, blockhash);
    client.send_and_confirm_transaction(&tx).await.map(|s| s.to_string()).map_err(|e| e.to_string())
}

fn condition(spec: &str) -> Result<(u8, u8), String> {
    match spec {
        "home-win" => Ok((HOME_WIN, 0)),
        "away-win" => Ok((AWAY_WIN, 0)),
        "draw" => Ok((DRAW, 0)),
        "both-score" => Ok((BOTH_TEAMS_SCORE, 0)),
        s => {
            for (prefix, kind) in [
                ("goals-over:", TOTAL_GOALS_OVER),
                ("home-goals-over:", HOME_GOALS_OVER),
                ("away-goals-over:", AWAY_GOALS_OVER),
                ("home-win-by-over:", HOME_WIN_BY_OVER),
                ("away-win-by-over:", AWAY_WIN_BY_OVER),
            ] {
                if let Some(n) = s.strip_prefix(prefix).and_then(|n| n.parse().ok()) {
                    return Ok((kind, n));
                }
            }
            Err(format!("unknown condition '{s}'"))
        }
    }
}

async fn run(command: &str, args: Args) -> Result<(), String> {
    let client = RpcClient::new(std::env::var("RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:8899".into()));
    match command {
        "pubkey" => println!("{}", args.keypair("keypair")?.pubkey()),
        "addresses" => {
            println!("core program:     {}", core_program());
            println!("oracle program:   {}", oracle_program());
            println!("config:           {}", config_address());
            println!("oracle authority: {}", oracle_authority_address());
        }
        "oracle-init" => {
            let root = args.keypair("root")?;
            println!("{}", send(&client, &[&root], &[oracle_init(&root.pubkey())]).await?);
        }
        "attest" => {
            let root = args.keypair("root")?;
            let candidate = args.key("candidate")?;
            let ix = match args.get("program")? {
                "oracle" => oracle_attest(&root.pubkey(), None, &candidate),
                "core" => core_attest(&root.pubkey(), None, &candidate),
                other => return Err(format!("--program must be oracle or core, not {other}")),
            };
            println!("{}", send(&client, &[&root], &[ix]).await?);
        }
        "config-init" => {
            let (a, b) = (args.keypair("root-a")?, args.keypair("root-b")?);
            let dials = ConfigDials {
                market_init_fee: args.num("init-fee", MIN_SEED)?,
                virtual_liquidity_multiple: args.num("k", VIRTUAL_LIQUIDITY_MULTIPLE)?,
                dispute_secs: args.num("dispute-secs", MIN_DISPUTE_SECS)?,
                fee_bps: args.num("fee-bps", 50)?,
                rake_bps: args.num("rake-bps", 100)?,
                max_in_bps_of_vault: args.num("max-in-bps", MAX_PRICE_IMPACT_BPS)?,
                fee_share_protocol_bps: 4_000,
                fee_share_backers_bps: 3_000,
                fee_share_creator_bps: 2_000,
            fee_share_platform_bps: 1_000,
                max_outcomes: MAX_OUTCOMES as u8,
                dbc_backing_enabled: args.num("dbc", 0u8)?,
                _padding: [0; 4],
            };
            let ix = config_init(&a.pubkey(), &b.pubkey(), dials, &args.key("mint")?, &args.key("treasury")?);
            println!("{}", send(&client, &[&a, &b], &[ix]).await?);
        }
        "create-market" => {
            let creator = args.keypair("creator")?;
            let mint = args.key("mint")?;
            let nonce: u64 = args.num("nonce", 0)?;
            let (kind, param) = condition(args.get("condition")?)?;
            let text = Text {
                question: args.get("question")?,
                rubric_yes: args.get("rubric-yes")?,
                rubric_no: args.get("rubric-no")?,
                label_yes: args.get("label-yes")?,
                label_no: args.get("label-no")?,
                category: args.opt("category").unwrap_or("Sports"),
                league: args.opt("league").unwrap_or("Football"),
            };
            check_text(&text)?;
            let market = market_address(&creator.pubkey(), nonce);
            let open = initialize_market(&creator.pubkey(), &mint, &args.key("oracle")?, nonce, kind, param);
            println!("market created: {}", send(&client, &[&creator], &[open]).await?);
            // The text is its own transaction: it can't fit in the same 1,232 bytes.
            let meta = set_metadata(&creator.pubkey(), &market, &text)?;
            println!("metadata set:   {}", send(&client, &[&creator], &[meta]).await?);
            println!("market:         {market}");
        }
        "create-backed-market" => {
            let creator = args.keypair("creator")?;
            let mint = args.key("mint")?;
            let nonce: u64 = args.num("nonce", 0)?;
            let (kind, param) = condition(args.get("condition")?)?;
            let text = Text {
                question: args.get("question")?,
                rubric_yes: args.get("rubric-yes")?,
                rubric_no: args.get("rubric-no")?,
                label_yes: args.get("label-yes")?,
                label_no: args.get("label-no")?,
                category: args.opt("category").unwrap_or("Sports"),
                league: args.opt("league").unwrap_or("Football"),
            };
            check_text(&text)?;
            let seed: f64 = args.num("seed", 30.0)?;
            let seed_atoms = (seed * 1_000_000.0).round() as u64;
            let raise_secs: i64 = args.num("raise-secs", 1200)?;
            let now = client.get_block_time(client.get_slot().await.map_err(|e| e.to_string())?).await.map_err(|e| e.to_string())?;
            let platform = args.opt("platform").map(|p| p.parse::<Pubkey>().map_err(|_| "--platform is not a valid address".to_string())).transpose()?.unwrap_or_default();
            let market = market_address(&creator.pubkey(), nonce);
            let open = initialize_backed_market(&creator.pubkey(), &mint, &args.key("oracle")?, nonce, kind, param, &platform, &args.key("backer-mint")?, seed_atoms, now + raise_secs);
            println!("market created: {}", send(&client, &[&creator], &[open]).await?);
            let meta = set_metadata(&creator.pubkey(), &market, &text)?;
            println!("metadata set:   {}", send(&client, &[&creator], &[meta]).await?);
            println!("market:         {market}");
            println!("vault:          {}", vault_address(&market));
            println!("raise deadline: {}", now + raise_secs);
        }
        "activate" => {
            let op = args.keypair("operator")?;
            let market = args.key("market")?;
            let data = client.get_account_data(&market).await.map_err(|e| e.to_string())?;
            let mk: metis_core::amm::state::Market = bytemuck::pod_read_unaligned(&data);
            let backer_mint = Pubkey::new_from_array(mk.backer_mint.to_bytes());
            println!("{}", send(&client, &[&op], &[activate_market(&op.pubkey(), &market, &backer_mint)]).await?);
        }
        "cancel-pending" => {
            let who = args.keypair("signer")?;
            println!("{}", send(&client, &[&who], &[cancel_pending_market(&who.pubkey(), &args.key("market")?)]).await?);
        }
        "claim-backer" => {
            let backer = args.keypair("backer")?;
            let market = args.key("market")?;
            let data = client.get_account_data(&market).await.map_err(|e| e.to_string())?;
            let mk: metis_core::amm::state::Market = bytemuck::pod_read_unaligned(&data);
            let backer_mint = Pubkey::new_from_array(mk.backer_mint.to_bytes());
            let ix = claim_backer(&backer.pubkey(), &market, &args.key("mint")?, &backer_mint);
            println!("{}", send(&client, &[&backer], &[ix]).await?);
        }
        "bet" => {
            let bettor = args.keypair("bettor")?;
            let side: u8 = args.num("side", 0)?;
            let usdc: f64 = args.num("usdc", 1.0)?;
            let atoms = (usdc * 1_000_000.0).round() as u64;
            let ix = place_bet(&bettor.pubkey(), &args.key("market")?, &args.key("mint")?, side, atoms, 0);
            println!("{}", send(&client, &[&bettor], &[ix]).await?);
        }
        "claim" | "refund" => {
            let holder = args.keypair("holder")?;
            let (market, mint, side) = (args.key("market")?, args.key("mint")?, args.num("side", 0u8)?);
            let ix = if command == "claim" { claim(&holder.pubkey(), &market, &mint, side) } else { claim_refund(&holder.pubkey(), &market, &mint, side) };
            println!("{}", send(&client, &[&holder], &[ix]).await?);
        }
        "creator-claim" => {
            let creator = args.keypair("creator")?;
            let ix = creator_claim(&creator.pubkey(), &args.key("market")?, &args.key("mint")?);
            println!("{}", send(&client, &[&creator], &[ix]).await?);
        }
        "forfeit" => {
            let (a, b) = (args.keypair("root-a")?, args.keypair("root-b")?);
            let ix = forfeit(&a.pubkey(), &b.pubkey(), &args.key("market")?, &args.key("mint")?, &args.key("treasury")?);
            println!("{}", send(&client, &[&a, &b], &[ix]).await?);
        }
        "settle" | "cancel" => {
            let who = args.keypair("signer")?;
            let (market, oracle) = (args.key("market")?, args.key("oracle")?);
            let ix = if command == "settle" { settle(&who.pubkey(), &market, &oracle) } else { cancel(&who.pubkey(), &market, &oracle) };
            println!("{}", send(&client, &[&who], &[ix]).await?);
        }
        "show" => {
            // What is on chain for one market: its state, pot, what is owed, and its book addresses.
            let market = args.key("market")?;
            let data = client.get_account_data(&market).await.map_err(|e| e.to_string())?;
            if data.len() != core::mem::size_of::<metis_core::amm::state::Market>() {
                return Err("that account is not a market".into());
            }
            let m: metis_core::amm::state::Market = bytemuck::pod_read_unaligned(&data);
            let status = metis_core::amm::state::MarketStatus::try_from(m.status).map(|s| format!("{s:?}")).unwrap_or_else(|_| format!("unknown({})", m.status));
            println!("market:        {market}");
            println!("status:        {status}   winning outcome: {}", m.winning_outcome);
            println!("kickoff:       {}   primary_end: {}   resolved_at: {}", m.bet_close_ts, m.primary_end, m.resolved_at);
            println!("pot:           {}   fees accrued: {}   payout pool: {}", m.total_collected, m.fees_accrued, m.payout_pool);
            if m.status == metis_core::amm::state::MarketStatus::Pending as u8 || m.seed_target > 0 {
                println!("raise:         seed target {}   deadline {}   backer mint {}   backer supply {}", m.seed_target, m.raise_deadline, Pubkey::new_from_array(m.backer_mint.to_bytes()), m.backer_supply);
            }
            println!("owed:          protocol {}   creator {}   platform {}   backers {}   forfeited {}", m.protocol_owed, m.creator_owed, m.platform_owed, m.backers_owed, m.forfeited_creator_fees);
            println!("shares:        yes {}   no {}", m.outcome_shares[0], m.outcome_shares[1]);
            for side in 0..2u8 {
                let book = book_address(&market, side);
                let exists = client.get_account_data(&book).await.is_ok();
                println!("book {side}:        {book} ({})", if exists { "created" } else { "not created" });
            }
        }
        "market-json" => {
            // One line of JSON for scripts (the Meteora keeper reads this).
            let market = args.key("market")?;
            let data = client.get_account_data(&market).await.map_err(|e| e.to_string())?;
            if data.len() != core::mem::size_of::<metis_core::amm::state::Market>() {
                return Err("that account is not a market".into());
            }
            let m: metis_core::amm::state::Market = bytemuck::pod_read_unaligned(&data);
            let pk = |a: &metis_core::amm::state::Market| Pubkey::new_from_array(a.vault.to_bytes());
            println!(
                "{}",
                serde_json::json!({
                    "market": market.to_string(),
                    "status": m.status,
                    "statusName": metis_core::amm::state::MarketStatus::try_from(m.status).map(|s| format!("{s:?}")).unwrap_or_default(),
                    "seedSource": m.seed_source,
                    "seedTarget": m.seed_target.to_string(),
                    "raiseDeadline": m.raise_deadline,
                    "betCloseTs": m.bet_close_ts,
                    "primaryEnd": m.primary_end,
                    "backerMint": Pubkey::new_from_array(m.backer_mint.to_bytes()).to_string(),
                    "backerSupply": m.backer_supply.to_string(),
                    "vault": pk(&m).to_string(),
                    "creator": Pubkey::new_from_array(m.creator.to_bytes()).to_string(),
                    "platform": Pubkey::new_from_array(m.platform.to_bytes()).to_string(),
                    "oracle": Pubkey::new_from_array(m.oracle.to_bytes()).to_string(),
                    "quoteMint": Pubkey::new_from_array(m.quote_mint.to_bytes()).to_string(),
                    "totalCollected": m.total_collected.to_string(),
                    "payoutPool": m.payout_pool.to_string(),
                    "protocolOwed": m.protocol_owed.to_string(),
                    "creatorOwed": m.creator_owed.to_string(),
                    "platformOwed": m.platform_owed.to_string(),
                    "backersOwed": m.backers_owed.to_string(),
                    "winningOutcome": m.winning_outcome,
                })
            );
        }
        "pause" => {
            let root = args.keypair("root")?;
            println!("{}", send(&client, &[&root], &[pause(&root.pubkey())]).await?);
        }
        "unpause" => {
            // The guardian first, then two different roots.
            let (g, a, b) = (args.keypair("guardian")?, args.keypair("root-a")?, args.keypair("root-b")?);
            let ix = unpause(&[g.pubkey(), a.pubkey(), b.pubkey()]);
            println!("{}", send(&client, &[&g, &a, &b], &[ix]).await?);
        }
        "maker-ask" => {
            let maker = args.keypair("maker")?;
            let ixs = maker_ask(&maker.pubkey(), &args.key("market")?, args.num("side", 0u8)?, args.num("base", 0u64)?, args.num("cents", 50u32)?);
            println!("{}", send(&client, &[&maker], &ixs).await?);
        }
        "sign-send" => {
            // Signs an unsigned wire transaction (as the aggregator returns it, base64) as its first
            // signer and sends it. For trying the aggregator's transactions by hand.
            use base64::Engine;
            let who = args.keypair("signer")?;
            let mut tx = base64::engine::general_purpose::STANDARD.decode(args.get("tx")?).map_err(|e| e.to_string())?;
            let count = tx[0] as usize; // fewer than 128 signatures: one length byte
            let message = tx[1 + 64 * count..].to_vec();
            tx[1..65].copy_from_slice(who.sign_message(&message).as_ref());
            let encoded = base64::engine::general_purpose::STANDARD.encode(&tx);
            let sig: String = client
                .send(solana_rpc_client_api::request::RpcRequest::SendTransaction, serde_json::json!([encoded, {"encoding": "base64", "preflightCommitment": "confirmed"}]))
                .await
                .map_err(|e| e.to_string())?;
            println!("{sig}");
        }
        other => return Err(format!("unknown command '{other}'. See the header of tools/metis-ops/src/main.rs")),
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = argv.first() else {
        eprintln!("usage: metis-ops <command> [--flag value]...  (see the header of tools/metis-ops/src/main.rs)");
        std::process::exit(2);
    };
    let result = match flags(&argv[1..]) {
        Ok(f) => run(command, Args(f)).await,
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
