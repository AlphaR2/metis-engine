//! Creating a Metis market from the aggregator: two unsigned transactions, because the market's
//! question, rules and labels don't fit in the same 1,232 bytes as the market itself.
//!
//!   step `create`   (default)  `initialize_market`: seeds the pot from the creator's USDC and
//!                              opens betting. Returns `venue_ref = create:<market>`.
//!                              With `venue_params.seed = "dbc"` it calls `initialize_backed_market`
//!                              instead: the creator pays no seed, the market opens `Pending`, and a
//!                              Meteora raise (`backer_mint`, `seed_usdc`, `raise_secs`) funds it.
//!   step `metadata` (`market`) `set_metadata`: the text the catalog shows. Creator only, once.
//!
//! The kickoff comes from the oracle fixture (which must exist already, scheduled, and far enough
//! ahead), so `start_time`, `end_time` and `resolution_time` on the intent are not used. The
//! signing wallet pays the seed and earns the creator share of fees.

use bytemuck::bytes_of;
use metis_core::{
    amm::{
        condition,
        instructions::{
            initialize_market::{InitializeBackedMarketArgs, InitializeMarketArgs},
            set_metadata::SetMetadataArgs,
        },
        metadata::{CATEGORY_MAX, LABEL_MAX, LEAGUE_MAX, QUESTION_MAX, RUBRIC_MAX},
        state::{INITIALIZE_BACKED_MARKET, INITIALIZE_MARKET, SET_METADATA},
    },
    constants::{MARKET_SEED, MAX_BACKED_SEED, METADATA_SEED, VAULT_SEED},
};
use serde_json::Value;
use solana_pubkey::Pubkey;

use super::{
    service::MetisAdapter,
    view::{associated_token_account, outcome_mint, usdc},
};
use crate::{
    errors::AdapterError,
    models::execution::{CostLine, CreateMarketIntent, IntentKind, TransactionCost, UnsignedTransaction},
    utils::{
        solana_tx::{ASSOCIATED_TOKEN_PROGRAM, Instruction, SYSTEM_PROGRAM, TOKEN_PROGRAM},
        time::now_unix,
    },
};

const ASSUMED_BLOCKHASH_SECS: i64 = 60;

/// The oracle program's football account discriminator (its last 8 bytes).
const FOOTBALL_SETTLED_DISCRIMINATOR: [u8; 8] = [130, 33, 121, 117, 222, 174, 224, 194];

/// What `create_step` has already worked out and `backed_create` needs.
struct BackedCreate {
    market: Pubkey,
    vault: Pubkey,
    nonce: u64,
    oracle: Pubkey,
    quote_mint: Pubkey,
    kind: u8,
    line: u8,
    min_seed: u64,
}

fn bad(message: impl Into<String>) -> AdapterError {
    AdapterError::InvalidRequest(message.into())
}

fn key(pubkey: &Pubkey) -> [u8; 32] {
    pubkey.to_bytes()
}

fn meta(pubkey: &Pubkey, signer: bool, writable: bool) -> crate::utils::solana_tx::AccountMeta {
    crate::utils::solana_tx::AccountMeta { pubkey: key(pubkey), is_signer: signer, is_writable: writable }
}

fn param<'a>(params: &'a Value, name: &str) -> Option<&'a str> {
    params.get(name).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// `home_win`, `away_win`, `draw`, `both_teams_score`, `total_goals_over:<n>`, or a one-side line:
/// `home_goals_over:<n>`, `away_goals_over:<n>`, `home_win_by_over:<n>`, `away_win_by_over:<n>`
/// ("more than n", so `home_win_by_over:3` is a margin of four or more).
pub(super) fn parse_condition(text: &str) -> Result<(u8, u8), AdapterError> {
    match text.trim() {
        "home_win" => Ok((condition::HOME_WIN, 0)),
        "away_win" => Ok((condition::AWAY_WIN, 0)),
        "draw" => Ok((condition::DRAW, 0)),
        "both_teams_score" => Ok((condition::BOTH_TEAMS_SCORE, 0)),
        other => {
            let with_line = |prefix: &str| other.strip_prefix(prefix).and_then(|n| n.parse::<u8>().ok());
            if let Some(n) = with_line("total_goals_over:").filter(|n| *n <= condition::MAX_GOAL_LINE) {
                return Ok((condition::TOTAL_GOALS_OVER, n));
            }
            for (prefix, kind) in [
                ("home_goals_over:", condition::HOME_GOALS_OVER),
                ("away_goals_over:", condition::AWAY_GOALS_OVER),
                ("home_win_by_over:", condition::HOME_WIN_BY_OVER),
                ("away_win_by_over:", condition::AWAY_WIN_BY_OVER),
            ] {
                if let Some(n) = with_line(prefix).filter(|n| *n <= condition::MAX_TEAM_LINE) {
                    return Ok((kind, n));
                }
            }
            Err(bad(format!(
                "condition '{other}' is not one of home_win, away_win, draw, both_teams_score, \
                 total_goals_over:<0-{}>, home_goals_over:<0-{team}>, away_goals_over:<0-{team}>, \
                 home_win_by_over:<0-{team}>, away_win_by_over:<0-{team}>",
                condition::MAX_GOAL_LINE,
                team = condition::MAX_TEAM_LINE
            )))
        }
    }
}

/// The product the market is created through, which earns the platform's share of its revenue:
/// `venue_params.platform`, a wallet address. Optional: with none, that share stays with the protocol.
pub(super) fn platform_param(params: &Value) -> Result<[u8; 32], AdapterError> {
    match param(params, "platform") {
        None => Ok([0u8; 32]),
        Some(text) => text
            .parse::<Pubkey>()
            .map(|p| p.to_bytes())
            .map_err(|_| bad("venue_params.platform is not a valid address")),
    }
}

fn put<const N: usize>(name: &str, text: &str) -> Result<([u8; N], u16), AdapterError> {
    if text.is_empty() || text.len() > N || text.chars().any(char::is_control) {
        return Err(bad(format!("{name} must be 1 to {N} bytes with no control characters (it is {} bytes)", text.len())));
    }
    let mut buf = [0u8; N];
    buf[..text.len()].copy_from_slice(text.as_bytes());
    Ok((buf, text.len() as u16))
}

impl MetisAdapter {
    pub(super) async fn build_create_market_transaction(&self, intent: &CreateMarketIntent) -> Result<UnsignedTransaction, AdapterError> {
        let wallet: Pubkey = intent.wallet.trim().parse().map_err(|_| bad("wallet is not a valid address"))?;
        let params = intent.venue_params.clone().unwrap_or(Value::Null);
        match param(&params, "step").unwrap_or("create") {
            "create" => self.create_step(intent, &params, &wallet).await,
            "metadata" => self.metadata_step(intent, &params, &wallet).await,
            other => Err(bad(format!("venue_params.step must be 'create' or 'metadata', not '{other}'"))),
        }
    }

    async fn create_step(&self, intent: &CreateMarketIntent, params: &Value, wallet: &Pubkey) -> Result<UnsignedTransaction, AdapterError> {
        // Fail early on text the second step would reject, so no one pays a seed for a market that
        // can never be described.
        self.text_args(intent, params)?;

        let oracle = param(params, "oracle")
            .map(str::to_string)
            .or_else(|| intent.sources_of_truth.iter().find_map(|s| s.strip_prefix("oracle:").map(str::to_string)))
            .ok_or_else(|| bad("a Metis market needs its oracle: venue_params.oracle or a sources_of_truth entry 'oracle:<address>'"))?;
        let oracle: Pubkey = oracle.trim().parse().map_err(|_| bad("the oracle is not a valid address"))?;
        let (kind, line) = parse_condition(param(params, "condition").ok_or_else(|| bad("venue_params.condition is required"))?)?;

        // Nothing about the match is taken on trust: the oracle must exist and be the program's.
        let oracle_data = self.rpc.account(&oracle).await?.ok_or_else(|| bad("that oracle account does not exist"))?;
        // The program checks owner, size and discriminator itself; this only gives a clear message
        // before the creator is asked to sign something that would fail.
        if oracle_data.len() < 8 || oracle_data[oracle_data.len() - 8..] != FOOTBALL_SETTLED_DISCRIMINATOR {
            return Err(bad("that account is not a football oracle"));
        }

        let dials = self.dials().await?;
        let program = self.settings.program_id;
        let nonce = params.get("nonce").and_then(Value::as_u64).unwrap_or_else(|| now_unix() as u64);
        let (market, _) = Pubkey::find_program_address(&[MARKET_SEED, wallet.as_ref(), &nonce.to_le_bytes()], &program);
        if self.rpc.account(&market).await?.is_some() {
            return Err(bad("a market with this wallet and nonce already exists; pass a different venue_params.nonce"));
        }
        let (vault, _) = Pubkey::find_program_address(&[VAULT_SEED, market.as_ref()], &program);

        let config = self.config_address();
        let config_data = self.rpc.account(&config).await?.ok_or_else(|| bad("the Metis config account does not exist"))?;
        let quote_mint = {
            use core::mem::offset_of;
            let at = offset_of!(metis_core::config::state::Config, quote_mint);
            Pubkey::new_from_array(config_data.get(at..at + 32).and_then(|b| b.try_into().ok()).ok_or_else(|| bad("the config account is unreadable"))?)
        };
        let creator_usdc = associated_token_account(wallet, &quote_mint);
        if param(params, "seed") == Some("dbc") {
            return self.backed_create(intent, params, wallet, BackedCreate { market, vault, nonce, oracle, quote_mint, kind, line, min_seed: dials.market_init_fee }).await;
        }
        let balance = self.rpc.account(&creator_usdc).await?.map(|d| super::chain::token_amount(&d)).unwrap_or(0);
        if balance < dials.market_init_fee {
            return Err(bad(format!(
                "creating a market seeds its pot with {} USDC from the creator's USDC account; this wallet holds {}",
                usdc(dials.market_init_fee),
                usdc(balance)
            )));
        }

        let platform = platform_param(params)?;
        let args = InitializeMarketArgs { nonce, num_outcomes: 2, condition_kind: kind, condition_param: line, _padding: [0; 5], platform };
        let mut data = INITIALIZE_MARKET.to_vec();
        data.extend_from_slice(bytes_of(&args));
        let (m0, m1) = (outcome_mint(&program, &market, 0), outcome_mint(&program, &market, 1));
        let token = Pubkey::from_str_const(TOKEN_PROGRAM);
        let ata_program = Pubkey::from_str_const(ASSOCIATED_TOKEN_PROGRAM);
        let system = Pubkey::from_str_const(SYSTEM_PROGRAM);
        let ix = Instruction {
            program_id: key(&program),
            accounts: vec![
                meta(&market, false, true),
                meta(&vault, false, true),
                meta(wallet, true, true),
                meta(&creator_usdc, false, true),
                meta(&quote_mint, false, false),
                meta(&config, false, false),
                meta(&oracle, false, false),
                meta(&system, false, false),
                meta(&token, false, false),
                meta(&ata_program, false, false),
                meta(&m0, false, true),
                meta(&associated_token_account(wallet, &m0), false, true),
                meta(&m1, false, true),
                meta(&associated_token_account(wallet, &m1), false, true),
            ],
            data,
        };
        let (transaction, last_valid_block_height) = self.finish(wallet, vec![ix]).await?;
        Ok(UnsignedTransaction {
            venue_id: "metis".to_string(),
            kind: IntentKind::CreateMarket,
            transaction,
            venue_ref: format!("create:{market}"),
            expires_at: now_unix() + ASSUMED_BLOCKHASH_SECS,
            required_signers: vec![intent.wallet.clone()],
            last_valid_block_height: Some(last_valid_block_height),
            cost: TransactionCost {
                total_usdc: usdc(dials.market_init_fee),
                lines: vec![CostLine {
                    label: "Market seed".to_string(),
                    amount_usdc: usdc(dials.market_init_fee),
                    note: Some("becomes the market's opening pot; you hold the shares it buys and earn the creator share of fees".to_string()),
                }],
            },
            preview: None,
        })
    }

    /// `initialize_backed_market`: the creator signs, pays rent only, and a Meteora raise seeds the pot.
    async fn backed_create(&self, intent: &CreateMarketIntent, params: &Value, wallet: &Pubkey, b: BackedCreate) -> Result<UnsignedTransaction, AdapterError> {
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

    async fn metadata_step(&self, intent: &CreateMarketIntent, params: &Value, wallet: &Pubkey) -> Result<UnsignedTransaction, AdapterError> {
        let market: Pubkey = param(params, "market").ok_or_else(|| bad("venue_params.market is required for the metadata step"))?.parse().map_err(|_| bad("venue_params.market is not a valid address"))?;
        let (_, on_chain, _) = self.load_market(&market.to_string()).await?;
        if Pubkey::new_from_array(on_chain.creator.to_bytes()) != *wallet {
            return Err(bad("only the market's creator can write its text"));
        }
        let args = self.text_args(intent, params)?;
        let program = self.settings.program_id;
        let (metadata, _) = Pubkey::find_program_address(&[METADATA_SEED, market.as_ref()], &program);
        if self.rpc.account(&metadata).await?.is_some() {
            return Err(bad("this market's text has already been written (it can only be written once)"));
        }
        let mut data = SET_METADATA.to_vec();
        data.extend_from_slice(bytes_of(&args));
        let system = Pubkey::from_str_const(SYSTEM_PROGRAM);
        let ix = Instruction {
            program_id: key(&program),
            accounts: vec![meta(wallet, true, true), meta(&market, false, false), meta(&metadata, false, true), meta(&system, false, false)],
            data,
        };
        let (transaction, last_valid_block_height) = self.finish(wallet, vec![ix]).await?;
        Ok(UnsignedTransaction {
            venue_id: "metis".to_string(),
            kind: IntentKind::CreateMarket,
            transaction,
            venue_ref: format!("create:{market}"),
            expires_at: now_unix() + ASSUMED_BLOCKHASH_SECS,
            required_signers: vec![intent.wallet.clone()],
            last_valid_block_height: Some(last_valid_block_height),
            cost: TransactionCost { total_usdc: rust_decimal::Decimal::ZERO, lines: Vec::new() },
            preview: None,
        })
    }

    /// The text of the market, validated against the program's own limits.
    fn text_args(&self, intent: &CreateMarketIntent, params: &Value) -> Result<SetMetadataArgs, AdapterError> {
        let rubric_no = param(params, "rubric_no").ok_or_else(|| bad("venue_params.rubric_no (what makes NO win, in words) is required"))?;
        let label_yes = param(params, "label_yes").ok_or_else(|| bad("venue_params.label_yes is required"))?;
        let label_no = param(params, "label_no").ok_or_else(|| bad("venue_params.label_no is required"))?;
        let league = param(params, "league").unwrap_or("Football");
        let (question, a) = put::<QUESTION_MAX>("question", intent.question.trim())?;
        let (rubric_yes, b) = put::<RUBRIC_MAX>("resolution_rule (the YES rubric)", intent.resolution_rule.trim())?;
        let (rubric_no, c) = put::<RUBRIC_MAX>("rubric_no", rubric_no)?;
        let (label_yes, d) = put::<LABEL_MAX>("label_yes", label_yes)?;
        let (label_no, e) = put::<LABEL_MAX>("label_no", label_no)?;
        let (category, f) = put::<CATEGORY_MAX>("category", intent.category.trim())?;
        let (league, g) = put::<LEAGUE_MAX>("league", league)?;
        Ok(SetMetadataArgs { question, rubric_yes, rubric_no, label_yes, label_no, category, league, lens: [a, b, c, d, e, f, g] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_is_optional_and_must_be_an_address() {
        let none = serde_json::json!({});
        assert_eq!(platform_param(&none).unwrap(), [0u8; 32], "no platform means the all-zero address");
        let wallet = Pubkey::new_unique();
        let named = serde_json::json!({ "platform": wallet.to_string() });
        assert_eq!(platform_param(&named).unwrap(), wallet.to_bytes());
        assert!(platform_param(&serde_json::json!({ "platform": "not-an-address" })).is_err());
    }

    #[test]
    fn every_condition_the_program_knows_parses_and_nothing_else_does() {
        assert_eq!(parse_condition("home_win").unwrap(), (condition::HOME_WIN, 0));
        assert_eq!(parse_condition(" away_win ").unwrap(), (condition::AWAY_WIN, 0));
        assert_eq!(parse_condition("draw").unwrap(), (condition::DRAW, 0));
        assert_eq!(parse_condition("both_teams_score").unwrap(), (condition::BOTH_TEAMS_SCORE, 0));
        assert_eq!(parse_condition("total_goals_over:2").unwrap(), (condition::TOTAL_GOALS_OVER, 2));
        assert_eq!(
            parse_condition(&format!("total_goals_over:{}", condition::MAX_GOAL_LINE)).unwrap(),
            (condition::TOTAL_GOALS_OVER, condition::MAX_GOAL_LINE)
        );
        assert_eq!(parse_condition("home_goals_over:3").unwrap(), (condition::HOME_GOALS_OVER, 3));
        assert_eq!(parse_condition("away_goals_over:0").unwrap(), (condition::AWAY_GOALS_OVER, 0));
        assert_eq!(parse_condition("home_win_by_over:3").unwrap(), (condition::HOME_WIN_BY_OVER, 3));
        assert_eq!(
            parse_condition(&format!("away_win_by_over:{}", condition::MAX_TEAM_LINE)).unwrap(),
            (condition::AWAY_WIN_BY_OVER, condition::MAX_TEAM_LINE)
        );
        for bad in [
            "", "home", "total_goals_over", "total_goals_over:x", "total_goals_over:200", "HOME_WIN",
            "home_goals_over:15", "home_win_by_over:15", "away_win_by_over", "away_goals_over:-1",
        ] {
            assert!(parse_condition(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn text_is_checked_against_the_programs_own_limits() {
        assert!(put::<24>("label", "Team A wins").is_ok());
        assert!(put::<24>("label", "").is_err(), "empty");
        assert!(put::<24>("label", &"x".repeat(25)).is_err(), "too long");
        assert!(put::<24>("label", "line\nbreak").is_err(), "control characters");
        let (buf, len) = put::<8>("label", "abc").unwrap();
        assert_eq!((len, &buf[..3], buf[3]), (3, &b"abc"[..], 0));
    }
}
