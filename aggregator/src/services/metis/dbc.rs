//! Open Pool in the aggregator: everything the Metis adapter does for a crowd-seeded market (a market whose
//! opening pot is raised on a Meteora bonding curve). See `dbc/README.md` for the whole flow.
//!
//! - [`MetisAdapter::backed_create`]: builds the `initialize_backed_market` transaction (the creator pays no seed).
//! - [`backer_tokens`](MetisAdapter::backer_tokens), [`backer_claim_ix`]: a backer redeems their token with
//!   `claim_backer`, in the same claim call as winnings and refunds.
//! - [`backer_position`]: a backer's token is shown as a "backer" position in the portfolio.
//!
//! The instruction layout and the discriminators come from the `metis-dbc` crate and `metis_core::dbc`.

use bytemuck::bytes_of;
use metis_core::{
    amm::state::{Market, MarketStatus, SeedSource},
    constants::MAX_BACKED_SEED,
    dbc::{CLAIM_BACKER, INITIALIZE_BACKED_MARKET},
};
use metis_dbc::InitializeBackedMarketArgs;
use serde_json::Value;
use solana_pubkey::Pubkey;

use super::{
    chain::token_amount,
    create::{ASSUMED_BLOCKHASH_SECS, bad, key, meta, param, platform_param},
    service::{MetisAdapter, position_of},
    view::{ID_YES, associated_token_account, outcome_mint, status_of, usdc},
};
use crate::{
    errors::AdapterError,
    models::{
        execution::{CostLine, CreateMarketIntent, IntentKind, TransactionCost, UnsignedTransaction, claim_ref},
        position::{Position, PositionState},
    },
    utils::{
        solana_tx::{Instruction, SYSTEM_PROGRAM, TOKEN_PROGRAM},
        time::now_unix,
    },
};

/// The backer token of a crowd-seeded market, or `None` for an ordinary creator-seeded one.
pub(super) fn backer_mint(market: &Market) -> Option<Pubkey> {
    let mint = Pubkey::new_from_array(market.backer_mint.to_bytes());
    (market.seed_source == SeedSource::DbcBackers as u8 && mint != Pubkey::default()).then_some(mint)
}

/// `claim_backer`: burns all of the wallet's backer tokens for its slice of the escrowed seed shares.
pub(super) fn backer_claim_ix(program: &Pubkey, market_address: &Pubkey, market: &Market, wallet: &Pubkey) -> Instruction {
    let backer_mint = Pubkey::new_from_array(market.backer_mint.to_bytes());
    let quote_mint = Pubkey::new_from_array(market.quote_mint.to_bytes());
    let (m0, m1) = (outcome_mint(program, market_address, 0), outcome_mint(program, market_address, 1));
    Instruction {
        program_id: key(program),
        accounts: vec![
            meta(market_address, false, true),
            meta(&Pubkey::new_from_array(market.vault.to_bytes()), false, true),
            meta(wallet, true, false),
            meta(&associated_token_account(wallet, &quote_mint), false, true),
            meta(&quote_mint, false, false),
            meta(&backer_mint, false, true),
            meta(&associated_token_account(wallet, &backer_mint), false, true),
            meta(&m0, false, true),
            meta(&m1, false, true),
            meta(&associated_token_account(market_address, &m0), false, true),
            meta(&associated_token_account(market_address, &m1), false, true),
            meta(&Pubkey::from_str_const(TOKEN_PROGRAM), false, false),
        ],
        data: CLAIM_BACKER.to_vec(),
    }
}

/// A backer's token as a portfolio position. It claims through the same claim call once the market settles or
/// is cancelled; what a backer gets depends on the whole escrow, so no payout figure is promised.
pub(super) fn backer_position(address: &Pubkey, market: &Market, tokens: u64, title: Option<String>, wallet: &str) -> Position {
    let mut position = position_of(address, market, ID_YES, tokens, title, wallet, false);
    position.side = "backer".to_string();
    position.venue_position_id = Some(format!("{address}:backer"));
    position.payout_usdc = None;
    position.state = match status_of(market) {
        Some(MarketStatus::Settled) => PositionState::Claimable,
        Some(MarketStatus::Cancelled) => PositionState::Cancelled,
        _ => PositionState::Open,
    };
    position.claim_ref = matches!(status_of(market), Some(MarketStatus::Settled | MarketStatus::Cancelled)).then(|| claim_ref(wallet, &address.to_string()));
    position
}

/// What `create_step` has already worked out and `backed_create` needs.
pub(super) struct BackedCreate {
    pub(super) market: Pubkey,
    pub(super) vault: Pubkey,
    pub(super) nonce: u64,
    pub(super) oracle: Pubkey,
    pub(super) quote_mint: Pubkey,
    pub(super) kind: u8,
    pub(super) line: u8,
    pub(super) min_seed: u64,
}

impl MetisAdapter {
    /// How many backer tokens the wallet holds in this market (0 for an ordinary market).
    pub(super) async fn backer_tokens(&self, wallet: &Pubkey, market: &Market) -> Result<u64, AdapterError> {
        let Some(mint) = backer_mint(market) else { return Ok(0) };
        Ok(self.rpc.account(&associated_token_account(wallet, &mint)).await?.map(|d| token_amount(&d)).unwrap_or(0))
    }

    /// `initialize_backed_market`: the creator signs, pays rent only, and a Meteora raise seeds the pot.
    pub(super) async fn backed_create(&self, intent: &CreateMarketIntent, params: &Value, wallet: &Pubkey, b: BackedCreate) -> Result<UnsignedTransaction, AdapterError> {
        let backer_mint: Pubkey = param(params, "backer_mint")
            .ok_or_else(|| bad("a crowd-seeded market needs venue_params.backer_mint (reserve it with the Meteora keeper first)"))?
            .parse()
            .map_err(|_| bad("venue_params.backer_mint is not a valid address"))?;
        let seed_usdc = params.get("seed_usdc").and_then(Value::as_f64).ok_or_else(|| bad("venue_params.seed_usdc (the opening pot the raise must deliver) is required"))?;
        let seed_target = (seed_usdc * 1_000_000.0).round() as u64;
        if seed_target < b.min_seed || seed_target > MAX_BACKED_SEED {
            return Err(bad(format!("seed_usdc must be between {} and {} USDC", usdc(b.min_seed), usdc(MAX_BACKED_SEED))));
        }
        let raise_secs = params.get("raise_secs").and_then(Value::as_i64).unwrap_or(1200);
        let raise_deadline = now_unix() + raise_secs;
        let platform = platform_param(params)?;
        let program = self.settings.program_id;
        let args = InitializeBackedMarketArgs {
            nonce: b.nonce,
            num_outcomes: 2,
            condition_kind: b.kind,
            condition_param: b.line,
            _padding: [0; 5],
            platform,
            backer_mint: backer_mint.to_bytes(),
            seed_target,
            raise_deadline,
        };
        let mut data = INITIALIZE_BACKED_MARKET.to_vec();
        data.extend_from_slice(bytes_of(&args));
        let token = Pubkey::from_str_const(TOKEN_PROGRAM);
        let system = Pubkey::from_str_const(SYSTEM_PROGRAM);
        let ix = Instruction {
            program_id: key(&program),
            accounts: vec![
                meta(&b.market, false, true),
                meta(&b.vault, false, true),
                meta(wallet, true, true),
                meta(&b.quote_mint, false, false),
                meta(&self.config_address(), false, false),
                meta(&b.oracle, false, false),
                meta(&system, false, false),
                meta(&token, false, false),
                meta(&outcome_mint(&program, &b.market, 0), false, true),
                meta(&outcome_mint(&program, &b.market, 1), false, true),
            ],
            data,
        };
        let (transaction, last_valid_block_height) = self.finish(wallet, vec![ix]).await?;
        Ok(UnsignedTransaction {
            venue_id: "metis".to_string(),
            kind: IntentKind::CreateMarket,
            transaction,
            venue_ref: format!("create:{}", b.market),
            expires_at: now_unix() + ASSUMED_BLOCKHASH_SECS,
            required_signers: vec![intent.wallet.clone()],
            last_valid_block_height: Some(last_valid_block_height),
            cost: TransactionCost {
                total_usdc: rust_decimal::Decimal::ZERO,
                lines: vec![CostLine {
                    label: "Open Pool".to_string(),
                    amount_usdc: usdc(seed_target),
                    note: Some(format!(
                        "you pay no seed: backers fill an Open Pool of {} USDC on a Meteora curve within {} minutes, or the market is cancelled",
                        usdc(seed_target),
                        raise_secs / 60
                    )),
                }],
            },
            preview: None,
        })
    }
}
