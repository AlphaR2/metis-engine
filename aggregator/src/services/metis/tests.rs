//! The adapter against a mock Solana RPC node that serves real account bytes, built with the
//! program's own types. The mock checks the filters the adapter sends against the real account
//! layout, so a wrong size or discriminator offset fails here, not on a live node.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{Json, Router, extract::State, routing::post};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytemuck::bytes_of;
use metis_core::{
    amm::{
        metadata::MarketMetadata,
        state::{MARKET_DISCRIMINATOR, Market, MarketStatus, PLACE_BET},
    },
    config::state::{CONFIG_DISCRIMINATOR, Config, ConfigDials},
    constants::{MIN_DISPUTE_SECS, MIN_SEED, VIRTUAL_LIQUIDITY_MULTIPLE},
    settlement::state::{CLAIM, CLAIM_BACKER, CLAIM_REFUND, SWEEP_FEES},
};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use solana_pubkey::Pubkey;

use super::{
    MetisAdapter, MetisSettings,
    view::{
        associated_token_account, default_config, outcome_mint,
        testing::{KICKOFF, market, metadata},
        usdc,
    },
};
use crate::{
    errors::AdapterError,
    models::{
        execution::{BuyIntent, ClaimIntent, IntentKind, claim_ref},
        position::PositionState,
        venue::ListParams,
    },
    services::VenueAdapter,
    utils::{
        solana_tx::{decode_32, inspect_transaction},
        time::freeze_now_for_this_thread,
    },
};

/// Before betting closes (primary_end is kickoff - 5h).
const NOW: i64 = KICKOFF - 24 * 3600;

#[derive(Default)]
struct Chain {
    markets: Vec<(Pubkey, Market)>,
    metadata: Vec<(Pubkey, MarketMetadata)>,
    accounts: HashMap<Pubkey, Vec<u8>>,
    /// owner -> [(mint, amount)]
    tokens: HashMap<Pubkey, Vec<(Pubkey, u64)>>,
    /// signature -> JSON status entry
    signatures: HashMap<String, Value>,
    log: Vec<String>,
}

type Shared = Arc<Mutex<Chain>>;

fn b64(data: &[u8]) -> Value {
    json!([BASE64.encode(data), "base64"])
}

async fn rpc(State(chain): State<Shared>, Json(req): Json<Value>) -> Json<Value> {
    let method = req["method"].as_str().unwrap_or_default().to_string();
    let params = req["params"].clone();
    let mut chain = chain.lock().unwrap();
    chain.log.push(method.clone());

    let result = match method.as_str() {
        "getProgramAccounts" => {
            // The adapter's filters must describe the real layout: size and discriminator offset.
            let filters = params[1]["filters"].as_array().expect("filters").clone();
            let size = filters[0]["dataSize"].as_u64().expect("dataSize") as usize;
            let offset = filters[1]["memcmp"]["offset"].as_u64().expect("memcmp offset") as usize;
            let wanted = BASE64.decode(filters[1]["memcmp"]["bytes"].as_str().unwrap()).unwrap();

            let all: Vec<(Pubkey, Vec<u8>)> = chain
                .markets
                .iter()
                .map(|(a, m)| (*a, bytes_of(m).to_vec()))
                .chain(chain.metadata.iter().map(|(a, m)| (*a, bytes_of(m).to_vec())))
                .collect();
            let rows: Vec<Value> = all
                .into_iter()
                .filter(|(_, d)| d.len() == size && d.get(offset..offset + wanted.len()) == Some(&wanted[..]))
                .map(|(a, d)| json!({ "pubkey": a.to_string(), "account": { "data": b64(&d), "lamports": 1 } }))
                .collect();
            Value::Array(rows)
        }
        "getAccountInfo" => {
            let address: Pubkey = params[0].as_str().unwrap().parse().unwrap();
            let data = chain
                .accounts
                .get(&address)
                .cloned()
                .or_else(|| chain.markets.iter().find(|(a, _)| *a == address).map(|(_, m)| bytes_of(m).to_vec()));
            json!({ "value": data.map(|d| json!({ "data": b64(&d) })) })
        }
        "getSlot" => json!(100),
        "getMultipleAccounts" => {
            let rows: Vec<Value> = params[0]
                .as_array()
                .unwrap()
                .iter()
                .map(|key| {
                    let address: Pubkey = key.as_str().unwrap().parse().unwrap();
                    let data = chain
                        .accounts
                        .get(&address)
                        .cloned()
                        .or_else(|| chain.markets.iter().find(|(a, _)| *a == address).map(|(_, m)| bytes_of(m).to_vec()));
                    data.map(|d| json!({ "data": b64(&d) })).unwrap_or(Value::Null)
                })
                .collect();
            json!({ "value": rows })
        }
        "getTokenAccountsByOwner" => {
            let owner: Pubkey = params[0].as_str().unwrap().parse().unwrap();
            let rows: Vec<Value> = chain
                .tokens
                .get(&owner)
                .into_iter()
                .flatten()
                .map(|(mint, amount)| {
                    json!({ "account": { "data": { "parsed": { "info": {
                        "mint": mint.to_string(), "tokenAmount": { "amount": amount.to_string() } } } } } })
                })
                .collect();
            json!({ "value": rows })
        }
        "getLatestBlockhash" => json!({ "value": { "blockhash": bs58::encode([7u8; 32]).into_string(), "lastValidBlockHeight": 424_242 } }),
        "getSignatureStatuses" => {
            let sig = params[0][0].as_str().unwrap();
            json!({ "value": [chain.signatures.get(sig).cloned().unwrap_or(Value::Null)] })
        }
        other => panic!("the mock RPC doesn't implement {other}"),
    };
    Json(json!({ "jsonrpc": "2.0", "id": 1, "result": result }))
}

struct World {
    adapter: MetisAdapter,
    chain: Shared,
    program: Pubkey,
    _server: tokio::task::JoinHandle<()>,
}

impl World {
    async fn start() -> Self {
        freeze_now_for_this_thread(NOW);
        let chain: Shared = Arc::default();
        let app = Router::new().route("/", post(rpc)).with_state(chain.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let program = Pubkey::new_unique();
        let adapter = MetisAdapter::new(MetisSettings {
            rpc_url: url,
            program_id: program,
            config: None,
            reference_deposit: MetisSettings::DEFAULT_REFERENCE_DEPOSIT,
        });
        let world = Self { adapter, chain, program, _server: server };
        world.set_config(ConfigDials { max_in_bps_of_vault: 1_000, ..dials() });
        world
    }

    fn set_config(&self, dials: ConfigDials) {
        // Config isn't plain-zeroable, so lay the account out by hand: the dials first, and the
        // discriminator where the program puts it.
        let mut data = vec![0u8; core::mem::size_of::<Config>()];
        data[..core::mem::size_of::<ConfigDials>()].copy_from_slice(bytes_of(&dials));
        let at = core::mem::offset_of!(Config, discriminator);
        data[at..at + 8].copy_from_slice(&CONFIG_DISCRIMINATOR);
        self.chain.lock().unwrap().accounts.insert(default_config(&self.program), data);
    }

    /// Adds a market with text; returns its address.
    fn add(&self, status: MarketStatus, edit: impl FnOnce(&mut Market)) -> Pubkey {
        let address = Pubkey::new_unique();
        let mut m = market(status);
        m.creator = Pubkey::new_unique().to_bytes().into();
        m.quote_mint = Pubkey::new_unique().to_bytes().into();
        m.vault = Pubkey::new_unique().to_bytes().into();
        edit(&mut m);
        let mut chain = self.chain.lock().unwrap();
        chain.markets.push((address, m));
        let text = metadata(&address);
        // Served at its PDA for single-account reads, and in the program-wide listing.
        let (pda, _) = Pubkey::find_program_address(&[metis_core::constants::METADATA_SEED, address.as_ref()], &self.program);
        chain.accounts.insert(pda, bytes_of(&text).to_vec());
        chain.metadata.push((pda, text));
        address
    }

    fn market(&self, address: &Pubkey) -> Market {
        self.chain.lock().unwrap().markets.iter().find(|(a, _)| a == address).unwrap().1
    }

    fn give(&self, owner: &Pubkey, mint: Pubkey, amount: u64) {
        self.chain.lock().unwrap().tokens.entry(*owner).or_default().push((mint, amount));
    }

    /// Adds a market at a chosen address (a real PDA, for tests that run the program), with text.
    fn add_at(&self, address: Pubkey, m: Market) {
        let mut chain = self.chain.lock().unwrap();
        chain.markets.retain(|(a, _)| *a != address);
        chain.markets.push((address, m));
        let text = metadata(&address);
        let (pda, _) = Pubkey::find_program_address(&[metis_core::constants::METADATA_SEED, address.as_ref()], &self.program);
        chain.accounts.insert(pda, bytes_of(&text).to_vec());
        chain.metadata.retain(|(a, _)| *a != pda);
        chain.metadata.push((pda, text));
    }

    fn give_account(&self, owner: &Pubkey, mint: &Pubkey, amount: u64) {
        // The wallet's token account itself, for the claim builder's direct balance read.
        let mut data = vec![0u8; 165];
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        self.chain.lock().unwrap().accounts.insert(associated_token_account(owner, mint), data);
    }
}

fn dials() -> ConfigDials {
    ConfigDials {
        market_init_fee: MIN_SEED,
        virtual_liquidity_multiple: VIRTUAL_LIQUIDITY_MULTIPLE,
        dispute_secs: MIN_DISPUTE_SECS,
        fee_bps: 50,
        rake_bps: 100,
        max_in_bps_of_vault: 1_000,
        fee_share_protocol_bps: 4_000,
        fee_share_backers_bps: 3_000,
        fee_share_creator_bps: 2_000,
            fee_share_platform_bps: 1_000,
        max_outcomes: 2,
        dbc_backing_enabled: 0,
        _padding: [0; 4],
    }
}

fn wallet() -> Pubkey {
    Pubkey::new_from_array([9; 32])
}

fn buy(market: &Pubkey, side: &str, amount: &str) -> BuyIntent {
    BuyIntent {
        venue_id: "metis".into(),
        venue_market_id: market.to_string(),
        side: side.into(),
        amount_usdc: amount.parse().unwrap(),
        wallet: wallet().to_string(),
        min_shares_out: None,
    }
}

fn claim(market: &Pubkey, kind: IntentKind) -> ClaimIntent {
    ClaimIntent {
        venue_id: "metis".into(),
        kind,
        venue_ref: claim_ref(&wallet().to_string(), &market.to_string()),
        wallet: wallet().to_string(),
    }
}

fn invalid(r: Result<impl std::fmt::Debug, AdapterError>) -> String {
    match r {
        Err(AdapterError::InvalidRequest(why)) => why,
        other => panic!("expected an InvalidRequest, got {other:?}"),
    }
}

// ── Catalog ──────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn the_catalog_lists_every_market_with_text_in_one_page() {
    let w = World::start().await;
    let open = w.add(MarketStatus::Open, |_| {});
    let settled = w.add(MarketStatus::Settled, |m| m.winning_outcome = 0);
    // A market whose creator never wrote the text: not listed.
    let bare = Pubkey::new_unique();
    w.chain.lock().unwrap().markets.push((bare, market(MarketStatus::Open)));

    let page = w.adapter.list_markets(ListParams::default()).await.unwrap();
    assert!(page.next_cursor.is_none());
    let mut ids: Vec<&str> = page.items.iter().map(|m| m.venue_market_id.as_str()).collect();
    ids.sort();
    let mut expected = vec![open.to_string(), settled.to_string()];
    expected.sort();
    assert_eq!(ids, expected, "listed: the two with text, not the bare one");

    // A later page has nothing: everything came in the first.
    let next = w.adapter.list_markets(ListParams { cursor: Some("x".into()), ..Default::default() }).await.unwrap();
    assert!(next.items.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn every_listed_market_meets_the_venue_contract() {
    let w = World::start().await;
    for status in [MarketStatus::Open, MarketStatus::Closed, MarketStatus::Settled, MarketStatus::Cancelled] {
        w.add(status, |m| m.winning_outcome = 1);
    }
    let page = w.adapter.list_markets(ListParams::default()).await.unwrap();
    assert_eq!(page.items.len(), 4);

    let mut seen = std::collections::HashSet::new();
    for m in &page.items {
        assert!(seen.insert(m.venue_market_id.clone()), "ids are unique");
        assert!(!m.venue_category.is_empty() && !m.title.is_empty());
        assert!(m.close_time < m.resolution_time, "betting closes before the market resolves");
        for price in [&m.yes_price, &m.no_price].into_iter().flatten() {
            let p: Decimal = price.parse().unwrap();
            assert!((Decimal::ZERO..=Decimal::ONE).contains(&p), "{price} is not a probability");
        }
        assert!(m.extra.contains_key("market") && m.extra.contains_key("oracle"), "unmodelled fields are kept");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_single_market_is_fetched_by_address_and_unknowns_are_not_found() {
    let w = World::start().await;
    let id = w.add(MarketStatus::Open, |_| {});
    let m = w.adapter.get_market(&id.to_string()).await.unwrap();
    assert_eq!(m.venue_market_id, id.to_string());
    assert_eq!(m.title, "Will Team A beat Team B?");

    for bogus in [Pubkey::new_unique().to_string(), "not-an-address".to_string()] {
        assert!(matches!(w.adapter.get_market(&bogus).await, Err(AdapterError::MarketNotFound(_))));
    }
}

// ── Quotes ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn a_quote_reports_what_the_bet_pays_so_size_times_price_is_the_stake() {
    let w = World::start().await;
    let id = w.add(MarketStatus::Open, |_| {});
    let q = w.adapter.quote(&id.to_string(), "yes", "40".parse().unwrap()).await.unwrap();

    let mk = w.market(&id);
    let expected = super::view::quote_side(&mk, 0, 40_000_000).unwrap();
    assert_eq!(q.venue, "metis");
    assert_eq!(q.effective_shares_or_size, usdc(expected.payout_if_win), "size is the payout, not the program's share count");
    assert_eq!(q.fee_usdc, usdc(expected.fee));
    let stake = q.effective_shares_or_size * q.avg_price;
    assert!((stake - Decimal::from(40)).abs() < Decimal::new(1, 3), "size x price is the amount staked, got {stake}");
    assert!(q.avg_price > Decimal::new(5, 1), "the honest break-even is above the curve's flat 50%: {}", q.avg_price);
    assert!(q.fully_filled);
}

#[tokio::test(flavor = "current_thread")]
async fn a_quote_refuses_what_the_program_would() {
    let w = World::start().await;
    let open = w.add(MarketStatus::Open, |_| {});
    let past = w.add(MarketStatus::Open, |m| m.primary_end = NOW - 1);
    let settled = w.add(MarketStatus::Settled, |_| {});

    assert!(invalid(w.adapter.quote(&open.to_string(), "draw", "5".parse().unwrap()).await).contains("yes/no"));
    assert!(invalid(w.adapter.quote(&open.to_string(), "yes", "0".parse().unwrap()).await).contains("positive"));
    assert!(invalid(w.adapter.quote(&open.to_string(), "yes", "0.0000001".parse().unwrap()).await).contains("6 decimal"));
    assert!(invalid(w.adapter.quote(&open.to_string(), "yes", "-4".parse().unwrap()).await).contains("positive"));
    // Past the primary window a market trades on its order books; with no book opened yet there is
    // nothing to quote against. Settled markets don't trade at all.
    assert!(invalid(w.adapter.quote(&past.to_string(), "yes", "5".parse().unwrap()).await).contains("not been opened"));
    assert!(invalid(w.adapter.quote(&settled.to_string(), "yes", "5".parse().unwrap()).await).contains("no longer trading"));
    assert!(matches!(
        w.adapter.quote(&Pubkey::new_unique().to_string(), "yes", "5".parse().unwrap()).await,
        Err(AdapterError::MarketNotFound(_))
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn a_market_on_the_books_with_no_book_yet_is_shown_but_never_routed_to() {
    let w = World::start().await;
    let id = w.add(MarketStatus::Open, |m| m.primary_end = NOW - 1);
    let m = w.adapter.get_market(&id.to_string()).await.unwrap();
    assert_eq!(m.phase, "secondary");
    assert!(m.display_only, "no readable book: shown, not routed");
    assert!(m.yes_price.is_none() && m.no_price.is_none());

    let sell = crate::models::execution::SellIntent {
        venue_id: "metis".into(),
        venue_ref: format!("{id}:yes"),
        wallet: wallet().to_string(),
    };
    // A wallet that holds nothing has nothing to sell, whatever the state of the books.
    assert!(invalid(w.adapter.build_sell(&sell).await).contains("holds no shares"));
    let garbage = crate::models::execution::SellIntent { venue_ref: "garbage".into(), ..sell.clone() };
    assert!(invalid(w.adapter.build_sell(&garbage).await).contains("not a Metis position"));
}

// ── Buying ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn a_buy_is_one_place_bet_instruction_signed_only_by_the_wallet() {
    let w = World::start().await;
    let id = w.add(MarketStatus::Open, |_| {});
    let mk = w.market(&id);
    let tx = w.adapter.build_buy(&buy(&id, "no", "25")).await.unwrap();

    assert_eq!(tx.venue_id, "metis");
    assert_eq!(tx.kind, IntentKind::BuyShares);
    assert_eq!(tx.required_signers, vec![wallet().to_string()]);
    assert_eq!(tx.last_valid_block_height, Some(424_242));
    assert_eq!(tx.expires_at, NOW + 60);
    assert!(tx.venue_ref.starts_with("metis:buy:") && tx.venue_ref.contains(&id.to_string()));

    let bytes = BASE64.decode(&tx.transaction).unwrap();
    let parsed = inspect_transaction(&bytes).unwrap();
    assert_eq!(parsed.num_required_signatures, 1, "the wallet is the only signer");
    assert!(parsed.signatures.iter().all(|s| s.iter().all(|b| *b == 0)), "unsigned: this service signs nothing");
    assert_eq!(parsed.static_keys[0], wallet().to_bytes(), "the wallet pays");
    assert_eq!(parsed.instructions.len(), 1);
    assert_eq!(parsed.lookup_tables, 0);

    let ix = &parsed.instructions[0];
    assert_eq!(parsed.static_keys[ix.program_index], w.program.to_bytes());
    assert_eq!(ix.data[..8], PLACE_BET);
    let args: &metis_core::amm::instructions::place_bet::PlaceBetArgs = bytemuck::from_bytes(&ix.data[8..]);
    assert_eq!((args.deposit_amount, args.outcome_index, args.min_shares_out), (25_000_000, 1, 0));

    // The accounts are exactly the ones the program expects, in its order, at the right addresses.
    let quote_mint = Pubkey::new_from_array(mk.quote_mint.to_bytes());
    let mint = outcome_mint(&w.program, &id, 1);
    let expected: Vec<[u8; 32]> = [
        id,
        Pubkey::new_from_array(mk.vault.to_bytes()),
        wallet(),
        associated_token_account(&wallet(), &quote_mint),
        quote_mint,
        mint,
        associated_token_account(&wallet(), &mint),
        default_config(&w.program),
    ]
    .iter()
    .map(Pubkey::to_bytes)
    .collect();
    let actual: Vec<[u8; 32]> = ix.account_indexes.iter().take(8).map(|i| parsed.static_keys[*i]).collect();
    assert_eq!(actual, expected);

    // The preview and cost match the quote.
    let q = super::view::quote_side(&mk, 1, 25_000_000).unwrap();
    let preview = tx.preview.unwrap();
    assert_eq!(preview.payout_if_win_usdc, Some(usdc(q.payout_if_win)));
    assert_eq!(tx.cost.total_usdc, Decimal::from(25));
    assert_eq!(tx.cost.lines.iter().map(|l| l.amount_usdc).sum::<Decimal>(), tx.cost.total_usdc, "the lines add up");
}

#[tokio::test(flavor = "current_thread")]
async fn a_floor_on_the_payout_becomes_the_same_fraction_of_shares_on_chain() {
    let w = World::start().await;
    let id = w.add(MarketStatus::Open, |_| {});
    let mk = w.market(&id);
    let q = super::view::quote_side(&mk, 0, 20_000_000).unwrap();
    let promised = usdc(q.payout_if_win);

    let min_shares = |tx: &crate::models::execution::UnsignedTransaction| {
        let parsed = inspect_transaction(&BASE64.decode(&tx.transaction).unwrap()).unwrap();
        let args: &metis_core::amm::instructions::place_bet::PlaceBetArgs = bytemuck::from_bytes(&parsed.instructions[0].data[8..]);
        args.min_shares_out
    };

    let mut intent = buy(&id, "yes", "20");
    let none = w.adapter.build_buy(&intent).await.unwrap();
    assert_eq!(min_shares(&none), 0);
    assert_eq!(none.preview.unwrap().max_slippage_bps, None);

    intent.min_shares_out = Some(promised);
    let exact = w.adapter.build_buy(&intent).await.unwrap();
    assert_eq!(min_shares(&exact), q.shares, "a floor equal to the quote demands every share");

    intent.min_shares_out = Some(promised / Decimal::from(2));
    let half = w.adapter.build_buy(&intent).await.unwrap();
    assert_eq!(min_shares(&half), q.shares / 2);
    assert_eq!(half.preview.unwrap().max_slippage_bps, Some(5_000));

    // A floor above the quote can never be met and is capped at demanding the quoted shares.
    intent.min_shares_out = Some(promised * Decimal::from(3));
    assert_eq!(min_shares(&w.adapter.build_buy(&intent).await.unwrap()), q.shares);
}

#[tokio::test(flavor = "current_thread")]
async fn a_buy_that_the_program_would_refuse_is_refused_with_a_plain_reason() {
    let w = World::start().await;
    let open = w.add(MarketStatus::Open, |_| {});
    let past = w.add(MarketStatus::Open, |m| m.primary_end = NOW - 1);

    assert!(invalid(w.adapter.build_buy(&buy(&past, "yes", "5")).await).contains("not been opened"));
    assert!(invalid(w.adapter.build_buy(&buy(&open, "maybe", "5")).await).contains("yes/no"));
    assert!(invalid(w.adapter.build_buy(&buy(&open, "yes", "0")).await).contains("positive"));
    let mut bad_wallet = buy(&open, "yes", "5");
    bad_wallet.wallet = "nope".into();
    assert!(invalid(w.adapter.build_buy(&bad_wallet).await).contains("wallet"));

    // A bet over the trade cap: told how much IS allowed, not just "no".
    w.set_config(ConfigDials { max_in_bps_of_vault: 10, ..dials() });
    let why = invalid(w.adapter.build_buy(&buy(&open, "yes", "100")).await);
    assert!(why.contains("largest single bet"), "{why}");

    // With the cap restored, a modest bet goes through and one over the cap is still refused.
    w.set_config(ConfigDials { max_in_bps_of_vault: 1_000, ..dials() });
    w.adapter.build_buy(&buy(&open, "yes", "20")).await.expect("a modest bet is fine");
    let why = invalid(w.adapter.build_buy(&buy(&open, "yes", "100")).await);
    assert!(why.contains("largest single bet"), "{why}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_missing_config_account_is_a_clear_error_not_a_bad_transaction() {
    let w = World::start().await;
    let id = w.add(MarketStatus::Open, |_| {});
    w.chain.lock().unwrap().accounts.remove(&default_config(&w.program));
    match w.adapter.build_buy(&buy(&id, "yes", "5")).await {
        Err(AdapterError::Venue(why)) => assert!(why.contains("config"), "{why}"),
        other => panic!("expected a Venue error, got {other:?}"),
    }
}

// ── Positions ────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn positions_follow_each_market_through_its_life() {
    let w = World::start().await;
    let open = w.add(MarketStatus::Open, |_| {});
    let won = w.add(MarketStatus::Settled, |m| {
        m.winning_outcome = 0;
        m.payout_pool = 25_000_000;
    });
    let lost = w.add(MarketStatus::Settled, |m| {
        m.winning_outcome = 1;
        m.payout_pool = 25_000_000;
    });
    let cancelled = w.add(MarketStatus::Cancelled, |m| m.payout_pool = 30_300_000);

    let me = wallet();
    w.give(&me, outcome_mint(&w.program, &open, 0), 7_000_000);
    w.give(&me, outcome_mint(&w.program, &won, 0), 3_000_000);
    w.give(&me, outcome_mint(&w.program, &lost, 0), 2_000_000);
    w.give(&me, outcome_mint(&w.program, &cancelled, 1), 5_000_000);
    w.give(&me, Pubkey::new_unique(), 99_000_000); // an unrelated token

    let positions = w.adapter.positions(&me.to_string()).await.unwrap();
    assert_eq!(positions.len(), 4, "only this program's outcome tokens count");
    let by_market = |id: &Pubkey| positions.iter().find(|p| p.venue_market_id == id.to_string()).unwrap();

    let p = by_market(&open);
    assert_eq!((p.state, p.side.as_str(), p.contracts), (PositionState::Open, "yes", usdc(7_000_000)));
    assert!(p.payout_usdc.is_some() && p.claim_ref.is_none());
    assert_eq!(p.title.as_deref(), Some("Will Team A beat Team B?"));

    let p = by_market(&won);
    assert_eq!(p.state, PositionState::Claimable);
    assert_eq!(p.claim_ref.as_deref(), Some(claim_ref(&me.to_string(), &won.to_string()).as_str()));
    assert!(p.payout_usdc.unwrap() > Decimal::ZERO);

    let p = by_market(&lost);
    assert_eq!((p.state, p.payout_usdc), (PositionState::Lost, None));
    assert!(p.claim_ref.is_none(), "nothing to claim on a loss");

    let p = by_market(&cancelled);
    assert_eq!(p.state, PositionState::Cancelled);
    assert!(p.claim_ref.is_some(), "a cancelled market is refunded through the same claim");
    assert!(p.cost_usdc.is_none() && p.avg_price_usdc.is_none(), "what a wallet paid isn't on chain, so it stays unknown");
}

#[tokio::test(flavor = "current_thread")]
async fn a_wallet_holding_nothing_has_no_positions_and_a_bad_wallet_is_refused() {
    let w = World::start().await;
    w.add(MarketStatus::Open, |_| {});
    assert!(w.adapter.positions(&wallet().to_string()).await.unwrap().is_empty());
    assert!(invalid(w.adapter.positions("nope").await).contains("wallet"));
}

// ── Claiming ─────────────────────────────────────────────────────────────

fn program_instructions(tx: &crate::models::execution::UnsignedTransaction, program: &Pubkey) -> Vec<(Vec<u8>, Vec<[u8; 32]>)> {
    let parsed = inspect_transaction(&BASE64.decode(&tx.transaction).unwrap()).unwrap();
    assert_eq!(parsed.num_required_signatures, 1);
    parsed
        .instructions
        .iter()
        .filter(|ix| parsed.static_keys[ix.program_index] == program.to_bytes())
        .map(|ix| (ix.data.clone(), ix.account_indexes.iter().map(|i| parsed.static_keys[*i]).collect()))
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn a_winner_claims_with_one_instruction_after_making_sure_their_usdc_account_exists() {
    let w = World::start().await;
    let id = w.add(MarketStatus::Settled, |m| {
        m.winning_outcome = 0;
        m.payout_pool = 25_000_000;
    });
    w.give_account(&wallet(), &outcome_mint(&w.program, &id, 0), 3_000_000);

    let tx = w.adapter.build_claim(&claim(&id, IntentKind::ClaimWinnings)).await.unwrap();
    assert_eq!(tx.kind, IntentKind::ClaimWinnings);
    let parsed = inspect_transaction(&BASE64.decode(&tx.transaction).unwrap()).unwrap();
    assert_eq!(parsed.instructions.len(), 2, "create the USDC account if missing, then claim");

    let metis = program_instructions(&tx, &w.program);
    assert_eq!(metis.len(), 1);
    assert_eq!(metis[0].0, CLAIM.to_vec());
    let mint = outcome_mint(&w.program, &id, 0);
    assert_eq!(metis[0].1[5], mint.to_bytes(), "claims against the winning mint");
    assert_eq!(metis[0].1[6], associated_token_account(&wallet(), &mint).to_bytes());
    assert!(tx.preview.unwrap().payout_if_win_usdc.unwrap() > Decimal::ZERO);
    assert_eq!(tx.cost.total_usdc, Decimal::ZERO);
}

#[tokio::test(flavor = "current_thread")]
async fn a_backer_with_no_shares_of_their_own_claims_through_claim_backer() {
    let w = World::start().await;
    let backer_mint = Pubkey::new_unique();
    let id = w.add(MarketStatus::Settled, |m| {
        m.winning_outcome = 0;
        m.payout_pool = 25_000_000;
        m.seed_source = metis_core::amm::state::SeedSource::DbcBackers as u8;
        m.backer_mint = backer_mint.to_bytes().into();
    });
    w.give_account(&wallet(), &backer_mint, 800_000_000_000);

    let tx = w.adapter.build_claim(&claim(&id, IntentKind::ClaimWinnings)).await.unwrap();
    let metis = program_instructions(&tx, &w.program);
    assert_eq!(metis.len(), 1, "no own shares, so only the backer redemption");
    assert_eq!(metis[0].0, CLAIM_BACKER.to_vec());
    assert_eq!(metis[0].1[5], backer_mint.to_bytes(), "burns the backer token");
    assert_eq!(metis[0].1[6], associated_token_account(&wallet(), &backer_mint).to_bytes());
    assert!(tx.preview.is_none(), "what a backer gets depends on the whole escrow, so no figure is promised");
}

#[tokio::test(flavor = "current_thread")]
async fn a_backer_who_also_holds_winning_shares_gets_both_claims_in_one_transaction() {
    let w = World::start().await;
    let backer_mint = Pubkey::new_unique();
    let id = w.add(MarketStatus::Settled, |m| {
        m.winning_outcome = 0;
        m.payout_pool = 25_000_000;
        m.seed_source = metis_core::amm::state::SeedSource::DbcBackers as u8;
        m.backer_mint = backer_mint.to_bytes().into();
    });
    w.give_account(&wallet(), &backer_mint, 5_000_000);
    w.give_account(&wallet(), &outcome_mint(&w.program, &id, 0), 3_000_000);

    let tx = w.adapter.build_claim(&claim(&id, IntentKind::ClaimWinnings)).await.unwrap();
    let metis = program_instructions(&tx, &w.program);
    assert_eq!(metis.iter().map(|(d, _)| d.clone()).collect::<Vec<_>>(), vec![CLAIM.to_vec(), CLAIM_BACKER.to_vec()]);
    assert!(tx.preview.unwrap().payout_if_win_usdc.unwrap() > Decimal::ZERO, "the wallet's own shares still show their payout");
}

#[tokio::test(flavor = "current_thread")]
async fn backer_tokens_show_as_a_position_that_is_claimable_once_settled() {
    let w = World::start().await;
    let backer_mint = Pubkey::new_unique();
    let raising = w.add(MarketStatus::Open, |m| {
        m.seed_source = metis_core::amm::state::SeedSource::DbcBackers as u8;
        m.backer_mint = backer_mint.to_bytes().into();
    });
    let me = wallet();
    w.give(&me, backer_mint, 800_000_000);
    let positions = w.adapter.positions(&me.to_string()).await.unwrap();
    assert_eq!(positions.len(), 1);
    assert_eq!((positions[0].side.as_str(), positions[0].state, positions[0].can_sell), ("backer", PositionState::Open, false));
    assert!(positions[0].claim_ref.is_none(), "nothing to claim while the market is open");
    assert_eq!(positions[0].venue_market_id, raising.to_string());

    w.chain.lock().unwrap().markets.iter_mut().for_each(|(_, m)| {
        m.status = MarketStatus::Settled as u8;
        m.winning_outcome = 0;
    });
    let positions = w.adapter.positions(&me.to_string()).await.unwrap();
    assert_eq!(positions[0].state, PositionState::Claimable);
    assert!(positions[0].claim_ref.is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn a_cancelled_market_is_refunded_for_each_side_the_wallet_holds() {
    let w = World::start().await;
    let id = w.add(MarketStatus::Cancelled, |m| m.payout_pool = 30_300_000);
    w.give_account(&wallet(), &outcome_mint(&w.program, &id, 0), 2_000_000);
    w.give_account(&wallet(), &outcome_mint(&w.program, &id, 1), 4_000_000);

    let tx = w.adapter.build_claim(&claim(&id, IntentKind::ClaimWinnings)).await.unwrap();
    let metis = program_instructions(&tx, &w.program);
    assert_eq!(metis.len(), 2, "one refund per side held");
    assert!(metis.iter().all(|(data, _)| data == &CLAIM_REFUND.to_vec()));
}

#[tokio::test(flavor = "current_thread")]
async fn a_claim_is_refused_when_there_is_nothing_to_collect() {
    let w = World::start().await;
    let open = w.add(MarketStatus::Open, |_| {});
    let settled = w.add(MarketStatus::Settled, |m| m.winning_outcome = 0);

    assert!(invalid(w.adapter.build_claim(&claim(&open, IntentKind::ClaimWinnings)).await).contains("not been settled"));
    assert!(invalid(w.adapter.build_claim(&claim(&settled, IntentKind::ClaimWinnings)).await).contains("no winning shares"));
    assert!(invalid(w.adapter.build_claim(&claim(&settled, IntentKind::BuyShares)).await).contains("claim"));

    // Someone else's claim reference cannot be used with this wallet.
    let mut stolen = claim(&settled, IntentKind::ClaimWinnings);
    stolen.venue_ref = claim_ref(&Pubkey::new_unique().to_string(), &settled.to_string());
    assert!(invalid(w.adapter.build_claim(&stolen).await).contains("different wallet"));
    let mut garbage = claim(&settled, IntentKind::ClaimWinnings);
    garbage.venue_ref = "garbage".into();
    assert!(invalid(w.adapter.build_claim(&garbage).await).contains("claim reference"));
}

#[tokio::test(flavor = "current_thread")]
async fn only_the_creator_can_claim_the_creator_fees_and_only_when_some_are_owed() {
    let w = World::start().await;
    let id = w.add(MarketStatus::Settled, |m| {
        m.creator = wallet().to_bytes().into();
        m.creator_owed = 120_000;
    });
    let tx = w.adapter.build_claim(&claim(&id, IntentKind::ClaimCreatorFees)).await.unwrap();
    assert_eq!(tx.kind, IntentKind::ClaimCreatorFees);
    let metis = program_instructions(&tx, &w.program);
    assert_eq!(&metis[0].0[..8], &SWEEP_FEES);
    assert_eq!(metis[0].0[8], 1, "the creator's share");
    assert_eq!(tx.preview.unwrap().payout_if_win_usdc, Some(usdc(120_000)));

    let owed_nothing = w.add(MarketStatus::Settled, |m| m.creator = wallet().to_bytes().into());
    assert!(invalid(w.adapter.build_claim(&claim(&owed_nothing, IntentKind::ClaimCreatorFees)).await).contains("no creator fees"));
    let someone_elses = w.add(MarketStatus::Settled, |m| m.creator_owed = 5);
    assert!(invalid(w.adapter.build_claim(&claim(&someone_elses, IntentKind::ClaimCreatorFees)).await).contains("only the market's creator"));
    let unsettled = w.add(MarketStatus::Open, |m| m.creator = wallet().to_bytes().into());
    assert!(invalid(w.adapter.build_claim(&claim(&unsettled, IntentKind::ClaimCreatorFees)).await).contains("settled"));
}

// ── Confirming ───────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn a_broadcast_is_confirmed_from_the_chain() {
    use crate::models::execution::ActionStatus;
    let w = World::start().await;
    let id = w.add(MarketStatus::Open, |_| {});
    let venue_ref = super::service::action_ref("buy", &id, &wallet().to_string());
    {
        let mut chain = w.chain.lock().unwrap();
        chain.signatures.insert("landed".into(), json!({ "confirmationStatus": "confirmed", "err": null }));
        chain.signatures.insert("seen".into(), json!({ "confirmationStatus": "processed", "err": null }));
        chain.signatures.insert("failed".into(), json!({ "confirmationStatus": "confirmed", "err": { "InstructionError": [0, { "Custom": 8 }] } }));
    }
    let landed = w.adapter.confirm(&venue_ref, "landed").await.unwrap();
    assert_eq!((landed.status, landed.venue_market_id), (ActionStatus::Confirmed, Some(id.to_string())));
    assert_eq!(w.adapter.confirm(&venue_ref, "seen").await.unwrap().status, ActionStatus::Submitted);
    assert_eq!(w.adapter.confirm(&venue_ref, "never-seen").await.unwrap().status, ActionStatus::Submitted);
    let failed = w.adapter.confirm(&venue_ref, "failed").await.unwrap();
    assert_eq!(failed.status, ActionStatus::Failed);
    assert!(failed.message.unwrap().contains("failed on chain"));
}

#[test]
fn the_venue_is_described_honestly() {
    let adapter = MetisAdapter::new(MetisSettings {
        rpc_url: "http://127.0.0.1:1".into(),
        program_id: Pubkey::new_unique(),
        config: None,
        reference_deposit: 1,
    });
    let caps = adapter.capabilities();
    assert_eq!(adapter.id(), "metis");
    assert!(caps.can_execute && caps.can_claim && caps.can_list_positions);
    assert!(caps.can_sell && caps.has_order_book, "shares trade on Manifest's books after betting closes");
    assert!(caps.can_create_markets, "creation is two transactions on an existing oracle fixture");
    let _ = (decode_32, MARKET_DISCRIMINATOR); // keep the helper imports used across configurations
}


// ── The adapter against the REAL program ─────────────────────────────────
//
// Everything above proves the adapter builds the instruction we expect. These prove the program
// accepts it: each transaction the adapter builds is signed with the wallet's key and executed on
// the compiled core program in litesvm, and the balances that result are checked against what the
// adapter promised. Needs `target/deploy/metis_core.so` (`cargo build-sbf`); skips loudly without.

mod real_program {
    use super::*;
    use litesvm::LiteSVM;
    use solana_account::Account;
    use solana_clock::Clock;
    use solana_keypair::Keypair;
    use solana_message::{MessageHeader, VersionedMessage, compiled_instruction::CompiledInstruction, v0};
    use solana_signer::Signer;
    use solana_transaction::versioned::VersionedTransaction;

    use metis_core::constants::{MARKET_SEED, OUTCOME_MINT_SEED, QUOTE_DECIMALS};

    const TOKEN: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

    /// An outcome mint as `initialize_market` creates it: the market is both mint and freeze
    /// authority (close_primary revokes both).
    fn pack_mint(authority: &Pubkey, supply: u64) -> Vec<u8> {
        let mut d = vec![0u8; 82];
        d[0..4].copy_from_slice(&1u32.to_le_bytes());
        d[4..36].copy_from_slice(authority.as_ref());
        d[36..44].copy_from_slice(&supply.to_le_bytes());
        d[44] = QUOTE_DECIMALS;
        d[45] = 1;
        d[46..50].copy_from_slice(&1u32.to_le_bytes());
        d[50..82].copy_from_slice(authority.as_ref());
        d
    }

    fn pack_token_account(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Vec<u8> {
        let mut d = vec![0u8; 165];
        d[0..32].copy_from_slice(mint.as_ref());
        d[32..64].copy_from_slice(owner.as_ref());
        d[64..72].copy_from_slice(&amount.to_le_bytes());
        d[108] = 1;
        d
    }

    fn balance(svm: &LiteSVM, account: &Pubkey) -> u64 {
        svm.get_account(account).map(|a| u64::from_le_bytes(a.data[64..72].try_into().unwrap())).unwrap_or(0)
    }

    /// The adapter's transaction bytes as a signed, executable transaction. The message is rebuilt
    /// field by field from the wire format (v0, no lookup tables), then signed by the wallet with
    /// the network's live blockhash.
    fn signed(bytes: &[u8], wallet: &Keypair, live_blockhash: solana_hash::Hash) -> VersionedTransaction {
        let mut pos = 0;
        let mut shortvec = |bytes: &[u8], pos: &mut usize| {
            let mut value = 0usize;
            for i in 0..3 {
                let b = bytes[*pos];
                *pos += 1;
                value |= usize::from(b & 0x7f) << (7 * i);
                if b & 0x80 == 0 {
                    break;
                }
            }
            value
        };
        let signatures = shortvec(bytes, &mut pos);
        pos += 64 * signatures;
        assert_eq!(bytes[pos], 0x80, "a v0 message");
        let header = MessageHeader {
            num_required_signatures: bytes[pos + 1],
            num_readonly_signed_accounts: bytes[pos + 2],
            num_readonly_unsigned_accounts: bytes[pos + 3],
        };
        pos += 4;
        let key_count = shortvec(bytes, &mut pos);
        let account_keys: Vec<Pubkey> = (0..key_count)
            .map(|i| Pubkey::new_from_array(bytes[pos + i * 32..pos + (i + 1) * 32].try_into().unwrap()))
            .collect();
        pos += key_count * 32;
        // The mock RPC hands out a made-up blockhash; a wallet signs with a live one. Nothing else in
        // the message is touched.
        let blockhash = live_blockhash;
        pos += 32;
        let instruction_count = shortvec(bytes, &mut pos);
        let mut instructions = Vec::new();
        for _ in 0..instruction_count {
            let program_id_index = bytes[pos];
            pos += 1;
            let n = shortvec(bytes, &mut pos);
            let accounts = bytes[pos..pos + n].to_vec();
            pos += n;
            let len = shortvec(bytes, &mut pos);
            let data = bytes[pos..pos + len].to_vec();
            pos += len;
            instructions.push(CompiledInstruction { program_id_index, accounts, data });
        }
        assert_eq!(shortvec(bytes, &mut pos), 0, "no address lookup tables");
        let message = v0::Message { header, account_keys, recent_blockhash: blockhash, instructions, address_table_lookups: vec![] };
        VersionedTransaction::try_new(VersionedMessage::V0(message), &[wallet]).expect("the wallet can sign it")
    }

    struct Run {
        w: World,
        svm: LiteSVM,
        wallet: Keypair,
        market_address: Pubkey,
        quote_mint: Pubkey,
        vault: Pubkey,
    }

    /// The program loaded in litesvm, a config, and a market at its real PDA - the same bytes served
    /// by the mock RPC, so the adapter and the program are looking at one world.
    async fn run(status: MarketStatus, creator_is_wallet: bool, edit: impl FnOnce(&mut Market)) -> Option<Run> {
        let so = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/deploy/metis_core.so");
        let mut svm = LiteSVM::new();
        let program = Pubkey::new_from_array(metis_core::ID.to_bytes());
        if svm.add_program_from_file(program, &so).is_err() {
            eprintln!("SKIPPED: {so:?} not found - run `cargo build-sbf --manifest-path programs/core/Cargo.toml`");
            return None;
        }
        let mut clock = svm.get_sysvar::<Clock>();
        clock.unix_timestamp = NOW;
        svm.set_sysvar(&clock);

        let mut w = World::start().await;
        // The adapter must point at the program that is actually loaded.
        w.adapter = MetisAdapter::new(MetisSettings {
            rpc_url: w.adapter.settings.rpc_url.clone(),
            program_id: program,
            config: None,
            reference_deposit: MetisSettings::DEFAULT_REFERENCE_DEPOSIT,
        });
        w.program = program;
        w.set_config(ConfigDials { max_in_bps_of_vault: 1_000, ..dials() });
        let config_bytes = w.chain.lock().unwrap().accounts[&default_config(&program)].clone();
        svm.set_account(default_config(&program), Account { lamports: 10_000_000, data: config_bytes, owner: program, ..Default::default() }).unwrap();

        let wallet = Keypair::new_from_array([5; 32]);
        let creator = if creator_is_wallet { wallet.pubkey() } else { Pubkey::new_unique() };
        let nonce = 7u64;
        let (market_address, bump) = Pubkey::find_program_address(&[MARKET_SEED, creator.as_ref(), &nonce.to_le_bytes()], &program);
        let quote_mint = Pubkey::new_unique();
        let vault = Pubkey::new_unique();
        let mut m = market(status);
        m.creator = creator.to_bytes().into();
        m.nonce = nonce;
        m.bump = bump;
        m.quote_mint = quote_mint.to_bytes().into();
        m.vault = vault.to_bytes().into();
        edit(&mut m);

        // The market, its vault holding exactly what the program expects, and both outcome mints.
        let mint_account = |lamports: u64, data: Vec<u8>, owner: Pubkey| Account { lamports, data, owner, ..Default::default() };
        svm.set_account(market_address, mint_account(10_000_000, bytes_of(&m).to_vec(), program)).unwrap();
        svm.set_account(quote_mint, mint_account(10_000_000, {
            let mut d = pack_mint(&Pubkey::new_unique(), 0);
            d[0..4].copy_from_slice(&0u32.to_le_bytes());
            d[46..50].copy_from_slice(&0u32.to_le_bytes());
            d
        }, TOKEN)).unwrap();
        let vault_balance = m.total_collected + m.fees_accrued;
        svm.set_account(vault, mint_account(10_000_000, pack_token_account(&quote_mint, &market_address, vault_balance), TOKEN)).unwrap();
        for side in 0..2 {
            let (mint, _) = Pubkey::find_program_address(&[OUTCOME_MINT_SEED, market_address.as_ref(), &[side as u8]], &program);
            svm.set_account(mint, mint_account(10_000_000, pack_mint(&market_address, m.outcome_shares[side]), TOKEN)).unwrap();
        }
        w.add_at(market_address, m);

        svm.airdrop(&wallet.pubkey(), 10_000_000_000).unwrap();
        Some(Run { w, svm, wallet, market_address, quote_mint, vault })
    }

    impl Run {
        fn fund_usdc(&mut self, amount: u64) -> Pubkey {
            let ata = associated_token_account(&self.wallet.pubkey(), &self.quote_mint);
            self.svm.set_account(ata, Account { lamports: 10_000_000, data: pack_token_account(&self.quote_mint, &self.wallet.pubkey(), amount), owner: TOKEN, ..Default::default() }).unwrap();
            ata
        }

        /// Gives the wallet shares of one side - in the program's world and in the adapter's.
        fn give_shares(&mut self, side: usize, amount: u64) {
            let mint = outcome_mint(&self.w.program, &self.market_address, side);
            let ata = associated_token_account(&self.wallet.pubkey(), &mint);
            self.svm.set_account(ata, Account { lamports: 10_000_000, data: pack_token_account(&mint, &self.wallet.pubkey(), amount), owner: TOKEN, ..Default::default() }).unwrap();
            self.w.give_account(&self.wallet.pubkey(), &mint, amount);
        }

        fn execute(&mut self, tx: &crate::models::execution::UnsignedTransaction) -> litesvm::types::TransactionResult {
            self.svm.expire_blockhash();
            let bytes = BASE64.decode(&tx.transaction).unwrap();
            let live = self.svm.latest_blockhash();
            self.svm.send_transaction(signed(&bytes, &self.wallet, live))
        }

        fn state(&self) -> Market {
            bytemuck::pod_read_unaligned(&self.svm.get_account(&self.market_address).unwrap().data)
        }
    }

    fn intent_buy(r: &Run, side: &str, amount: &str) -> BuyIntent {
        BuyIntent {
            venue_id: "metis".into(),
            venue_market_id: r.market_address.to_string(),
            side: side.into(),
            amount_usdc: amount.parse().unwrap(),
            wallet: r.wallet.pubkey().to_string(),
            min_shares_out: None,
        }
    }

    fn intent_claim(r: &Run, kind: IntentKind) -> ClaimIntent {
        ClaimIntent {
            venue_id: "metis".into(),
            kind,
            venue_ref: claim_ref(&r.wallet.pubkey().to_string(), &r.market_address.to_string()),
            wallet: r.wallet.pubkey().to_string(),
        }
    }

    /// A scheduled fixture at `kickoff`, in both the program's world and the adapter's.
    fn add_oracle(r: &mut Run, kickoff: i64) -> Pubkey {
        let address = Pubkey::new_unique();
        let state = oracle::instructions::football::FootballSettled::new(address.to_bytes(), kickoff, NOW);
        let oracle_program = Pubkey::new_from_array(oracle::ID.to_bytes());
        r.svm.set_account(address, Account { lamports: 1_000_000, data: state.as_bytes().to_vec(), owner: oracle_program, ..Default::default() }).unwrap();
        r.w.chain.lock().unwrap().accounts.insert(address, state.as_bytes().to_vec());
        address
    }

    /// Turns the crowd-seed switch on and points the config at `r.quote_mint`, in both worlds.
    fn enable_backing(r: &mut Run) {
        let program = r.w.program;
        let mut cfg = r.w.chain.lock().unwrap().accounts[&default_config(&program)].clone();
        let dbc_at = core::mem::offset_of!(Config, dials) + core::mem::offset_of!(ConfigDials, dbc_backing_enabled);
        cfg[dbc_at] = 1;
        let mint_at = core::mem::offset_of!(Config, quote_mint);
        cfg[mint_at..mint_at + 32].copy_from_slice(r.quote_mint.as_ref());
        r.w.chain.lock().unwrap().accounts.insert(default_config(&program), cfg.clone());
        r.svm.set_account(default_config(&program), Account { lamports: 10_000_000, data: cfg, owner: program, ..Default::default() }).unwrap();
    }

    fn backed_intent(r: &Run, oracle: &Pubkey, backer_mint: &Pubkey, seed_usdc: f64) -> crate::models::execution::CreateMarketIntent {
        serde_json::from_value(json!({
            "venue_id": "metis", "question": "Will Nigeria beat Ghana?", "resolution_rule": "YES if Nigeria lead after 90 minutes.",
            "sources_of_truth": [], "image_url": "https://example.test/x.png", "category": "sports", "start_time": 0, "end_time": 0, "resolution_time": 0,
            "wallet": r.wallet.pubkey().to_string(),
            "venue_params": { "seed": "dbc", "oracle": oracle.to_string(), "condition": "home_win", "label_yes": "Nigeria win", "label_no": "Draw or Ghana",
                "rubric_no": "NO otherwise.", "nonce": 99, "backer_mint": backer_mint.to_string(), "seed_usdc": seed_usdc, "raise_secs": 1200 }
        }))
        .expect("a valid create intent")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_crowd_seeded_market_the_adapter_builds_opens_pending_on_the_real_program() {
        let Some(mut r) = run(MarketStatus::Open, false, |_| {}).await else { return };
        enable_backing(&mut r);
        let oracle = add_oracle(&mut r, KICKOFF);
        let backer_mint = Pubkey::new_unique();
        let tx = r.w.adapter.build_create_market_transaction(&backed_intent(&r, &oracle, &backer_mint, 40.0)).await.unwrap();
        assert_eq!(tx.cost.total_usdc, Decimal::ZERO, "the creator pays no seed, only rent");
        r.execute(&tx).expect("the program accepts the adapter's initialize_backed_market");
        let (market, _) = Pubkey::find_program_address(&[metis_core::constants::MARKET_SEED, r.wallet.pubkey().as_ref(), &99u64.to_le_bytes()], &r.w.program);
        let m: Market = bytemuck::pod_read_unaligned(&r.svm.get_account(&market).unwrap().data);
        assert_eq!(m.status, MarketStatus::Pending as u8);
        assert_eq!(m.seed_target, 40_000_000);
        assert_eq!(Pubkey::new_from_array(m.backer_mint.to_bytes()), backer_mint);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_crowd_seeded_market_needs_its_backer_mint_and_a_seed_in_bounds() {
        let Some(mut r) = run(MarketStatus::Open, false, |_| {}).await else { return };
        enable_backing(&mut r);
        let oracle = add_oracle(&mut r, KICKOFF);
        let mut no_mint = backed_intent(&r, &oracle, &Pubkey::new_unique(), 40.0);
        no_mint.venue_params.as_mut().unwrap().as_object_mut().unwrap().remove("backer_mint");
        assert!(r.w.adapter.build_create_market_transaction(&no_mint).await.is_err(), "no backer mint, no market");
        for seed in [1.0, 100_001.0] {
            let intent = backed_intent(&r, &oracle, &Pubkey::new_unique(), seed);
            assert!(r.w.adapter.build_create_market_transaction(&intent).await.is_err(), "seed {seed} is out of bounds");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_buy_the_adapter_builds_executes_on_the_real_program_and_buys_what_was_quoted() {
        let Some(mut r) = run(MarketStatus::Open, false, |_| {}).await else { return };
        let usdc_ata = r.fund_usdc(500_000_000);
        let before = r.state();
        let promised = super::super::view::quote_side(&before, 1, 25_000_000).unwrap();

        let tx = r.w.adapter.build_buy(&intent_buy(&r, "no", "25")).await.unwrap();
        r.execute(&tx).expect("the program accepts the adapter's transaction");

        let mint = outcome_mint(&r.w.program, &r.market_address, 1);
        let got = balance(&r.svm, &associated_token_account(&r.wallet.pubkey(), &mint));
        assert_eq!(got, promised.shares, "the wallet got exactly the shares the quote said");
        assert_eq!(balance(&r.svm, &usdc_ata), 500_000_000 - 25_000_000, "and paid the gross amount");
        assert_eq!(balance(&r.svm, &r.vault), before.total_collected + before.fees_accrued + 25_000_000);

        let after = r.state();
        assert_eq!(after.total_collected, before.total_collected + promised.net);
        assert_eq!(after.fees_accrued, before.fees_accrued + promised.fee);
        assert_eq!(after.outcome_shares[1], before.outcome_shares[1] + promised.shares);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_slippage_floor_the_adapter_encodes_is_enforced_by_the_program() {
        let Some(mut r) = run(MarketStatus::Open, false, |_| {}).await else { return };
        r.fund_usdc(500_000_000);
        // Quote, then let the market move under the user before their transaction lands.
        let mut intent = intent_buy(&r, "yes", "20");
        let quote = super::super::view::quote_side(&r.state(), 0, 20_000_000).unwrap();
        intent.min_shares_out = Some(usdc(quote.payout_if_win));
        let tx = r.w.adapter.build_buy(&intent).await.unwrap();

        // Someone else buys the same side first, raising the price: fewer shares per dollar.
        let mut moved = r.state();
        moved.outcome_shares[0] += moved.outcome_shares[0] / 3;
        r.svm.set_account(r.market_address, Account { lamports: 10_000_000, data: bytes_of(&moved).to_vec(), owner: r.w.program, ..Default::default() }).unwrap();

        let result = r.execute(&tx);
        let err = result.expect_err("the program must refuse a bet that now buys fewer shares than the floor");
        assert!(format!("{:?}", err.err).contains(&format!("Custom({})", metis_core::error::MetisError::SlippageExceeded as u32)), "{:?}", err.err);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_winners_claim_executes_and_pays_exactly_what_the_adapter_showed() {
        let Some(mut r) = run(MarketStatus::Settled, false, |m| {
            m.winning_outcome = 0;
            m.payout_pool = 29_700_000;
        })
        .await
        else {
            return;
        };
        let shares = r.state().outcome_shares[0] / 3;
        r.give_shares(0, shares);
        let usdc_ata = r.fund_usdc(0);
        let shown = super::super::view::payout_atoms(&r.state(), 0, shares).unwrap();

        let tx = r.w.adapter.build_claim(&intent_claim(&r, IntentKind::ClaimWinnings)).await.unwrap();
        assert_eq!(tx.preview.as_ref().unwrap().payout_if_win_usdc, Some(usdc(shown)));
        r.execute(&tx).expect("the program accepts the adapter's claim");
        assert_eq!(balance(&r.svm, &usdc_ata), shown, "paid exactly what the adapter said it would");
        assert_eq!(balance(&r.svm, &associated_token_account(&r.wallet.pubkey(), &outcome_mint(&r.w.program, &r.market_address, 0))), 0, "the shares were burned");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_claim_creates_the_missing_usdc_account_on_the_way() {
        let Some(mut r) = run(MarketStatus::Settled, false, |m| {
            m.winning_outcome = 0;
            m.payout_pool = 29_700_000;
        })
        .await
        else {
            return;
        };
        let shares = r.state().outcome_shares[0] / 4;
        r.give_shares(0, shares);
        // No USDC account exists for the wallet at all.
        let usdc_ata = associated_token_account(&r.wallet.pubkey(), &r.quote_mint);
        assert!(r.svm.get_account(&usdc_ata).is_none());

        let tx = r.w.adapter.build_claim(&intent_claim(&r, IntentKind::ClaimWinnings)).await.unwrap();
        r.execute(&tx).expect("the transaction creates the account and then pays into it");
        assert!(balance(&r.svm, &usdc_ata) > 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_refund_on_a_cancelled_market_returns_money_for_both_sides() {
        let Some(mut r) = run(MarketStatus::Cancelled, false, |m| m.payout_pool = 30_150_000).await else { return };
        let (yes, no) = (r.state().outcome_shares[0] / 5, r.state().outcome_shares[1] / 5);
        r.give_shares(0, yes);
        r.give_shares(1, no);
        let usdc_ata = r.fund_usdc(0);
        let shown = super::super::view::payout_atoms(&r.state(), 0, yes).unwrap() + super::super::view::payout_atoms(&r.state(), 1, no).unwrap();

        let tx = r.w.adapter.build_claim(&intent_claim(&r, IntentKind::ClaimWinnings)).await.unwrap();
        r.execute(&tx).expect("both refunds execute in one transaction");
        assert_eq!(balance(&r.svm, &usdc_ata), shown);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_creator_claims_their_fees_through_the_adapter() {
        let Some(mut r) = run(MarketStatus::Settled, true, |m| {
            m.winning_outcome = 0;
            m.payout_pool = 29_700_000;
            m.creator_owed = 150_000;
        })
        .await
        else {
            return;
        };
        // The vault holds the pool and the owed fees, as the program leaves it after settlement.
        r.svm
            .set_account(r.vault, Account { lamports: 10_000_000, data: pack_token_account(&r.quote_mint, &r.market_address, 29_700_000 + 150_000), owner: TOKEN, ..Default::default() })
            .unwrap();
        let usdc_ata = r.fund_usdc(0);

        let tx = r.w.adapter.build_claim(&intent_claim(&r, IntentKind::ClaimCreatorFees)).await.unwrap();
        r.execute(&tx).expect("the program accepts the adapter's fee claim");
        assert_eq!(balance(&r.svm, &usdc_ata), 150_000);
        assert_eq!(r.state().creator_owed, 0, "and it can only be claimed once");
    }

    // ── Trading between holders, on the real Manifest books ─────────────────

    use manifest_client::{BatchUpdateParams, DepositParams, OrderType, PlaceOrderParams};

    fn mp(key: &Pubkey) -> manifest_client::Pubkey {
        manifest_client::Pubkey::new_from_array(key.to_bytes())
    }

    impl Run {
        fn send_signed(&mut self, ixs: &[solana_instruction::Instruction], payer: &Keypair) {
            self.svm.expire_blockhash();
            let tx = solana_transaction::Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &[payer], self.svm.latest_blockhash());
            self.svm.send_transaction(tx).expect("transaction should succeed");
        }

        /// Copies accounts from the program's world into the mock RPC's, so the adapter reads what
        /// the program just wrote.
        fn sync(&mut self, accounts: &[Pubkey]) {
            for address in accounts {
                let Some(account) = self.svm.get_account(address) else { continue };
                let mut chain = self.w.chain.lock().unwrap();
                if *address == self.market_address {
                    let market: Market = bytemuck::pod_read_unaligned(&account.data);
                    chain.markets.retain(|(a, _)| a != address);
                    chain.markets.push((*address, market));
                } else {
                    chain.accounts.insert(*address, account.data.clone());
                }
            }
        }

        fn book(&self, side: usize) -> Pubkey {
            self.w.adapter.book_address(&self.market_address, side)
        }

        /// Runs the program's own close_primary, which creates both books on the real Manifest.
        fn close_primary(&mut self) {
            let program = self.w.program;
            let payer = Keypair::new_from_array([77; 32]);
            self.svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();
            // Opening the books is an operator's job: attest this key, as two roots would have.
            let delegate = Pubkey::find_program_address(&[metis_core::constants::DELEGATED_AUTHORITY_SEED, payer.pubkey().as_ref()], &program).0;
            let mut attestation = vec![0u8; 112];
            attestation[0..32].copy_from_slice(payer.pubkey().as_ref());
            attestation[32..64].copy_from_slice(&[1; 32]);
            attestation[64..96].copy_from_slice(&[2; 32]);
            attestation[96..104].copy_from_slice(&1u64.to_le_bytes());
            attestation[104..112].copy_from_slice(&metis_core::config::state::DELEGATED_AUTHORITY_DISCRIMINATOR);
            self.svm.set_account(delegate, Account { lamports: 10_000_000, data: attestation, owner: program, ..Default::default() }).unwrap();
            let mints = [outcome_mint(&program, &self.market_address, 0), outcome_mint(&program, &self.market_address, 1)];
            let books = [self.book(0), self.book(1)];
            let manifest = Pubkey::new_from_array(manifest_client::MANIFEST_PROGRAM_ID.to_bytes());
            let vault_of = |book: &Pubkey, mint: &Pubkey| Pubkey::new_from_array(manifest_client::get_vault_address(&mp(book), &mp(mint)).0.to_bytes());
            let meta = |k: &Pubkey, signer: bool, writable: bool| solana_instruction::AccountMeta { pubkey: *k, is_signer: signer, is_writable: writable };
            let ix = solana_instruction::Instruction {
                program_id: program,
                accounts: vec![
                    meta(&self.market_address, false, true),
                    meta(&payer.pubkey(), true, true),
                    meta(&self.quote_mint, false, false),
                    meta(&mints[0], false, true),
                    meta(&mints[1], false, true),
                    meta(&books[0], false, true),
                    meta(&books[1], false, true),
                    meta(&vault_of(&books[0], &mints[0]), false, true),
                    meta(&vault_of(&books[0], &self.quote_mint), false, true),
                    meta(&vault_of(&books[1], &mints[1]), false, true),
                    meta(&vault_of(&books[1], &self.quote_mint), false, true),
                    meta(&Pubkey::from_str_const("11111111111111111111111111111111"), false, false),
                    meta(&TOKEN, false, false),
                    meta(&Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"), false, false),
                    meta(&manifest, false, false),
                    meta(&default_config(&program), false, false),
                    meta(&delegate, false, false),
                ],
                data: metis_core::amm::state::CLOSE_PRIMARY.to_vec(),
            };
            self.send_signed(&[ix], &payer);
            assert!(self.svm.get_account(&books[0]).is_some(), "close_primary created the books");
            self.sync(&[self.market_address, books[0], books[1]]);
        }

        /// A maker puts orders on one side's book. `price` is cents per slice.
        /// Asks are funded with slices (given to the maker here); bids with USDC.
        fn make(&mut self, side: usize, maker: &Keypair, is_bid: bool, orders: &[(u64, u32)]) {
            self.svm.airdrop(&maker.pubkey(), 10_000_000_000).unwrap();
            let book = self.book(side);
            let mint = outcome_mint(&self.w.program, &self.market_address, side);
            let (token, deposit_mint) = if is_bid { (self.quote_mint, self.quote_mint) } else { (mint, mint) };
            let total: u64 = if is_bid {
                orders.iter().map(|(base, cents)| *base * *cents as u64 / 100).sum()
            } else {
                orders.iter().map(|(base, _)| *base).sum()
            };
            let holding = associated_token_account(&maker.pubkey(), &token);
            self.svm.set_account(holding, Account { lamports: 10_000_000, data: pack_token_account(&token, &maker.pubkey(), total), owner: TOKEN, ..Default::default() }).unwrap();

            let mut batch = BatchUpdateParams::new();
            for (base, cents) in orders {
                // cents per slice = mantissa x 10^-2, in the book's own encoding.
                batch = batch.add_order(PlaceOrderParams::new(*base, *cents, -2, is_bid, OrderType::Limit));
            }
            let maker_pk = mp(&maker.pubkey());
            let ixs = [
                manifest_client::claim_seat_instruction(maker_pk, mp(&book)),
                manifest_client::deposit_instruction(maker_pk, mp(&book), mp(&holding), mp(&deposit_mint), mp(&TOKEN), DepositParams::new(total)),
                manifest_client::batch_update_instruction(maker_pk, mp(&book), batch),
            ];
            self.send_signed(&ixs, maker);
            self.sync(&[book]);
        }
    }

    /// A market whose betting has ended, with both books created by the real program and a maker
    /// selling YES slices at 70c (40 of them) and 80c (20 of them), and bidding 50c for 30.
    async fn on_the_books() -> Option<Run> {
        let mut r = run(MarketStatus::Open, false, |m| m.primary_end = NOW - 1).await?;
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/deploy/manifest.so");
        if r.svm.add_program_from_file(Pubkey::new_from_array(manifest_client::MANIFEST_PROGRAM_ID.to_bytes()), &manifest).is_err() {
            eprintln!("SKIPPED: {manifest:?} not found - run `solana program dump MNFSTqtC93rEfYHB6hF82sKdZpUDFWkViLByLd1k1Ms target/deploy/manifest.so --url mainnet-beta`");
            return None;
        }
        r.close_primary();
        let asker = Keypair::new_from_array([31; 32]);
        r.make(ID_YES, &asker, false, &[(40_000_000, 70), (20_000_000, 80)]);
        let bidder = Keypair::new_from_array([32; 32]);
        r.make(ID_YES, &bidder, true, &[(30_000_000, 50)]);
        Some(r)
    }

    use super::super::view::ID_YES;

    #[tokio::test(flavor = "current_thread")]
    async fn the_catalog_prices_a_market_on_the_books_from_its_cheapest_ask() {
        let Some(r) = on_the_books().await else { return };
        let m = r.w.adapter.get_market(&r.market_address.to_string()).await.unwrap();
        assert_eq!(m.phase, "secondary");
        assert!(!m.display_only, "a readable, tradable book: it is routed to");

        // A YES slice costs 70c and pays out the pot's share per slice if YES wins.
        let mk = r.state();
        let pays_per_slice = super::super::view::distributable(&mk) as f64 / mk.outcome_shares[0] as f64;
        let expected = 0.70 / pays_per_slice;
        let shown: f64 = m.yes_price.unwrap().parse().unwrap();
        assert!((shown - expected).abs() < 0.0005, "shown {shown}, expected {expected}");
        assert!(m.no_price.is_none(), "nobody is selling NO slices, so there is no NO price");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_quote_on_the_books_is_exact_because_the_pot_is_final() {
        let Some(r) = on_the_books().await else { return };
        let q = r.w.adapter.quote(&r.market_address.to_string(), "yes", "21".parse().unwrap()).await.unwrap();
        // $21 at 70c is exactly 30 slices.
        let mk = r.state();
        let payout = super::super::view::payout_atoms(&mk, 0, 30_000_000).unwrap();
        assert_eq!(q.effective_shares_or_size, usdc(payout));
        assert_eq!(q.fee_usdc, Decimal::ZERO, "no fee on a trade between holders");
        assert!(q.fully_filled);
        let stake = q.effective_shares_or_size * q.avg_price;
        assert!((stake - Decimal::from(21)).abs() < Decimal::new(1, 3), "{stake}");

        // More than the book holds: filled as far as it goes, and says so.
        let big = r.w.adapter.quote(&r.market_address.to_string(), "yes", "500".parse().unwrap()).await.unwrap();
        assert!(!big.fully_filled);
        // NO has no book liquidity at all.
        assert!(matches!(
            r.w.adapter.quote(&r.market_address.to_string(), "no", "5".parse().unwrap()).await,
            Err(AdapterError::NoLiquidity { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_buy_from_the_book_executes_on_the_real_manifest_and_delivers_the_quote() {
        let Some(mut r) = on_the_books().await else { return };
        let usdc_ata = r.fund_usdc(100_000_000);
        let tx = r.w.adapter.build_buy(&intent_buy(&r, "yes", "21")).await.unwrap();
        assert_eq!(tx.kind, IntentKind::BuyShares);
        assert_eq!(tx.cost.total_usdc, Decimal::from(21));
        r.execute(&tx).expect("Manifest accepts the adapter's swap");

        let mint = outcome_mint(&r.w.program, &r.market_address, 0);
        let got = balance(&r.svm, &associated_token_account(&r.wallet.pubkey(), &mint));
        assert_eq!(got, 30_000_000, "exactly the 30 slices $21 buys at 70c");
        assert_eq!(balance(&r.svm, &usdc_ata), 100_000_000 - 21_000_000);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_floor_on_a_book_buy_is_enforced_when_the_book_moves() {
        let Some(mut r) = on_the_books().await else { return };
        r.fund_usdc(100_000_000);
        let quote = r.w.adapter.quote(&r.market_address.to_string(), "yes", "21".parse().unwrap()).await.unwrap();
        let mut intent = intent_buy(&r, "yes", "21");
        intent.min_shares_out = Some(quote.effective_shares_or_size);
        let tx = r.w.adapter.build_buy(&intent).await.unwrap();

        // Before the user's transaction lands, someone buys out the cheap asks.
        let sniper = Keypair::new_from_array([33; 32]);
        r.svm.airdrop(&sniper.pubkey(), 10_000_000_000).unwrap();
        let usdc_ata = associated_token_account(&sniper.pubkey(), &r.quote_mint);
        let mint = outcome_mint(&r.w.program, &r.market_address, 0);
        r.svm.set_account(usdc_ata, Account { lamports: 10_000_000, data: pack_token_account(&r.quote_mint, &sniper.pubkey(), 1_000_000_000), owner: TOKEN, ..Default::default() }).unwrap();
        let slice_ata = associated_token_account(&sniper.pubkey(), &mint);
        r.svm.set_account(slice_ata, Account { lamports: 10_000_000, data: pack_token_account(&mint, &sniper.pubkey(), 0), owner: TOKEN, ..Default::default() }).unwrap();
        let swap = manifest_client::swap_instruction(
            mp(&sniper.pubkey()), mp(&r.book(0)), mp(&slice_ata), mp(&usdc_ata), mp(&mint), mp(&r.quote_mint), mp(&TOKEN),
            None, false, false, manifest_client::SwapParams::new(28_000_000, 0, false, true), false,
        );
        r.send_signed(&[swap], &sniper);

        let err = r.execute(&tx).expect_err("the floor must stop a buy that now fills far worse");
        println!("refused as it should be: {:?}", err.err);
        assert_eq!(balance(&r.svm, &associated_token_account(&r.wallet.pubkey(), &mint)), 0, "nothing was bought");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_position_bought_on_the_book_can_be_sold_back_into_the_bids() {
        let Some(mut r) = on_the_books().await else { return };
        let usdc_ata = r.fund_usdc(100_000_000);
        let buy = r.w.adapter.build_buy(&intent_buy(&r, "yes", "21")).await.unwrap();
        r.execute(&buy).unwrap();
        let mint = outcome_mint(&r.w.program, &r.market_address, 0);
        let slice_ata = associated_token_account(&r.wallet.pubkey(), &mint);
        r.w.give(&r.wallet.pubkey(), mint, balance(&r.svm, &slice_ata));
        r.w.give_account(&r.wallet.pubkey(), &mint, balance(&r.svm, &slice_ata));

        // The position is listed and sellable, now that there are bids to sell into.
        let positions = r.w.adapter.positions(&r.wallet.pubkey().to_string()).await.unwrap();
        assert_eq!(positions.len(), 1);
        assert!(positions[0].can_sell, "bids exist");
        let position_id = positions[0].venue_position_id.clone().unwrap();

        let before = balance(&r.svm, &usdc_ata);
        let sell = r.w.adapter.build_sell(&crate::models::execution::SellIntent {
            venue_id: "metis".into(),
            venue_ref: position_id,
            wallet: r.wallet.pubkey().to_string(),
        }).await.unwrap();
        assert_eq!(sell.kind, IntentKind::SellShares);
        let preview = sell.preview.clone().unwrap();
        assert_eq!(preview.max_slippage_bps, Some(200), "the venue's own margin, stated as such");
        r.execute(&sell).expect("Manifest accepts the adapter's sale");

        // 30 slices into a bid for 30 at 50c: $15.
        assert_eq!(balance(&r.svm, &usdc_ata) - before, 15_000_000);
        assert_eq!(balance(&r.svm, &slice_ata), 0, "all sold");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn selling_is_refused_while_betting_is_open_and_after_settlement() {
        let Some(mut r) = run(MarketStatus::Open, false, |_| {}).await else { return };
        let mint = outcome_mint(&r.w.program, &r.market_address, 0);
        r.give_shares(0, 5_000_000);
        let sell = crate::models::execution::SellIntent {
            venue_id: "metis".into(),
            venue_ref: format!("{}:yes", r.market_address),
            wallet: r.wallet.pubkey().to_string(),
        };
        assert!(invalid(r.w.adapter.build_sell(&sell).await).contains("isn't trading on the order book"), "betting is still open");

        // Once the market has settled there is nothing to sell into either: claim instead.
        let mut settled = r.state();
        settled.status = MarketStatus::Settled as u8;
        settled.winning_outcome = 0;
        settled.payout_pool = 29_700_000;
        r.svm.set_account(r.market_address, Account { lamports: 10_000_000, data: bytes_of(&settled).to_vec(), owner: r.w.program, ..Default::default() }).unwrap();
        r.sync(&[r.market_address]);
        r.w.give_account(&r.wallet.pubkey(), &mint, 5_000_000);
        assert!(invalid(r.w.adapter.build_sell(&sell).await).contains("isn't trading on the order book"), "it has settled");
    }

    // ── A maker's balances on the books ─────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn a_makers_resting_asks_count_as_a_position_even_with_an_empty_token_account() {
        let Some(r) = on_the_books().await else { return };
        let asker = Keypair::new_from_array([31; 32]);
        let positions = r.w.adapter.positions(&asker.pubkey().to_string()).await.unwrap();
        assert_eq!(positions.len(), 1, "the 60 slices offered on the YES book are still the maker's");
        assert_eq!(positions[0].side, "yes");
        assert_eq!(positions[0].contracts, usdc(60_000_000));
        assert_eq!(positions[0].open_orders, 2);
        assert!(!positions[0].can_sell, "slices on a book are not in the wallet, so there is nothing to swap");

        // A bidder holds USDC on the book, not slices: no position.
        let bidder = Keypair::new_from_array([32; 32]);
        assert!(r.w.adapter.positions(&bidder.pubkey().to_string()).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_makers_claim_withdraws_their_seat_first() {
        use base64::Engine as _;
        let Some(mut r) = on_the_books().await else { return };
        // The market settles with YES the winner.
        let mut settled = r.state();
        settled.status = MarketStatus::Settled as u8;
        settled.winning_outcome = 0;
        settled.payout_pool = 29_700_000;
        r.svm.set_account(r.market_address, Account { lamports: 10_000_000, data: bytes_of(&settled).to_vec(), owner: r.w.program, ..Default::default() }).unwrap();
        r.sync(&[r.market_address]);

        let asker = Keypair::new_from_array([31; 32]);
        let intent = |wallet: &Keypair| ClaimIntent {
            venue_id: "metis".into(),
            kind: IntentKind::ClaimWinnings,
            venue_ref: claim_ref(&wallet.pubkey().to_string(), &r.market_address.to_string()),
            wallet: wallet.pubkey().to_string(),
        };
        let tx = r.w.adapter.build_claim(&intent(&asker)).await.expect("a maker with only seat balances can claim");
        let bytes = base64::engine::general_purpose::STANDARD.decode(&tx.transaction).unwrap();
        let manifest = manifest_client::MANIFEST_PROGRAM_ID.to_bytes();
        assert!(bytes.windows(32).any(|w| w == manifest), "the claim carries a Manifest withdraw for the maker's seat");

        // A wallet with nothing anywhere still gets the plain refusal.
        let nobody = Keypair::new_from_array([99; 32]);
        assert!(invalid(r.w.adapter.build_claim(&intent(&nobody)).await).contains("holds no winning shares"));
    }
}
