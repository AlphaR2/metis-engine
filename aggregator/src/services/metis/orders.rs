//! Building the unsigned transactions: buying shares and claiming. Each returns bytes the user's
//! wallet signs; this service never signs and never broadcasts.
//!
//! What is built must be exactly what was asked for, and nothing else. Instructions go through the
//! same strict compiler Panta's use: the wallet is the only signer, every program must be on an
//! allowlist, no address lookup tables, and the result must fit a packet.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytemuck::bytes_of;
use metis_core::{
    amm::{
        curve::{cost, priced_shares},
        instructions::place_bet::PlaceBetArgs,
        state::{MarketStatus, SeedSource, PLACE_BET},
    },
    config::state::{Config, ConfigDials},
    constants::{MAX_PRICE_IMPACT_BPS, QUOTE_DECIMALS},
    settlement::{
        instructions::sweep_fees::{SWEEP_CREATOR, SweepFeesArgs},
        state::{CLAIM, CLAIM_BACKER, CLAIM_REFUND, SWEEP_FEES},
    },
};
use manifest_client::{MANIFEST_PROGRAM_ID, SwapParams, WithdrawParams, swap_instruction, withdraw_instruction};
use rust_decimal::Decimal;
use solana_pubkey::Pubkey;

use super::{
    chain::{decode_market, token_amount},
    secondary::{buy_with, sell_into, seat_holding},
    service::{MetisAdapter, action_ref, parse_position_ref},
    view::{
        ID_NO, ID_YES, associated_token_account, outcome_mint, payout_atoms, primary_open, quote_side,
        secondary_open, side_index, side_name, to_atoms, usdc,
    },
};
use crate::{
    errors::AdapterError,
    models::execution::{
        BuyIntent, ClaimIntent, CostLine, IntentKind, OrderPreview, SellIntent, TransactionCost, UnsignedTransaction,
        parse_claim_ref,
    },
    utils::{
        solana_tx::{
            AccountMeta, ASSOCIATED_TOKEN_PROGRAM, COMMON_PROGRAMS, CompileError, Instruction, SYSTEM_PROGRAM,
            TOKEN_PROGRAM, check_programs, compile_unsigned_v0,
        },
        time::now_unix,
    },
};

/// How long an unsigned transaction is treated as submittable. Solana blockhashes live about 60-90
/// seconds; the block height returned with the transaction is the real authority.
const ASSUMED_BLOCKHASH_SECS: i64 = 60;

/// The worst price a sale will accept, as a fall from the quoted proceeds. A sale has no
/// caller-supplied bound (`SellIntent` carries none), so this is the venue's own ceiling, stated in
/// the preview as such rather than presented as the user's choice.
const SELL_SLIPPAGE_BPS: i64 = 200;

fn key(pubkey: &Pubkey) -> [u8; 32] {
    pubkey.to_bytes()
}

fn meta(pubkey: &Pubkey, signer: bool, writable: bool) -> AccountMeta {
    AccountMeta { pubkey: key(pubkey), is_signer: signer, is_writable: writable }
}

fn parse_key(what: &str, text: &str) -> Result<Pubkey, AdapterError> {
    text.trim().parse().map_err(|_| AdapterError::InvalidRequest(format!("{what} is not a valid address")))
}

/// Creates a token account if it is missing, harmlessly and idempotently, paid for by `payer`.
fn create_ata_idempotent(payer: &Pubkey, ata: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Instruction {
    Instruction {
        program_id: key(&Pubkey::from_str_const(ASSOCIATED_TOKEN_PROGRAM)),
        accounts: vec![
            meta(payer, true, true),
            meta(ata, false, true),
            meta(owner, false, false),
            meta(mint, false, false),
            meta(&Pubkey::from_str_const(SYSTEM_PROGRAM), false, false),
            meta(&Pubkey::from_str_const(TOKEN_PROGRAM), false, false),
        ],
        data: vec![1], // CreateIdempotent
    }
}

/// A Manifest instruction in this service's own shape. Only the bytes cross over: the two crates
/// may be built against different versions of the Solana types.
fn from_manifest(ix: manifest_client::Instruction) -> Instruction {
    Instruction {
        program_id: ix.program_id.to_bytes(),
        accounts: ix
            .accounts
            .iter()
            .map(|a| AccountMeta { pubkey: a.pubkey.to_bytes(), is_signer: a.is_signer, is_writable: a.is_writable })
            .collect(),
        data: ix.data,
    }
}

fn compile_error(e: CompileError) -> AdapterError {
    AdapterError::Venue(e.to_string())
}

/// Reads the dials out of the Config account: after checking it is one.
fn decode_dials(data: &[u8]) -> Option<ConfigDials> {
    use core::mem::{offset_of, size_of};
    if data.len() != size_of::<Config>() {
        return None;
    }
    let disc_at = offset_of!(Config, discriminator);
    if data[disc_at..disc_at + 8] != metis_core::config::state::CONFIG_DISCRIMINATOR {
        return None;
    }
    Some(bytemuck::pod_read_unaligned(&data[..size_of::<ConfigDials>()]))
}

impl MetisAdapter {
    pub(super) async fn dials(&self) -> Result<ConfigDials, AdapterError> {
        let address = self.config_address();
        let data = self
            .rpc
            .account(&address)
            .await?
            .ok_or_else(|| AdapterError::Venue(format!("the Metis config account {address} does not exist - is METIS_PROGRAM_ID / METIS_CONFIG right?")))?;
        decode_dials(&data).ok_or_else(|| AdapterError::Parse(format!("{address} is not a Metis config account")))
    }

    fn allowed_programs(&self) -> Vec<String> {
        let mut allowed: Vec<String> = COMMON_PROGRAMS.iter().map(|p| p.to_string()).collect();
        allowed.push(self.settings.program_id.to_string());
        // Trades between holders go through Manifest's order books.
        allowed.push(MANIFEST_PROGRAM_ID.to_string());
        allowed
    }

    pub(super) async fn finish(
        &self,
        wallet: &Pubkey,
        instructions: Vec<Instruction>,
    ) -> Result<(String, u64), AdapterError> {
        let allowed = self.allowed_programs();
        let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
        check_programs(&instructions, &allowed).map_err(compile_error)?;
        let (blockhash, last_valid) = self.rpc.latest_blockhash().await?;
        let bytes = compile_unsigned_v0(key(wallet), &instructions, blockhash).map_err(compile_error)?;
        Ok((BASE64.encode(bytes), last_valid))
    }

    pub(super) async fn build_buy_transaction(&self, intent: &BuyIntent) -> Result<UnsignedTransaction, AdapterError> {
        let side = side_index(&intent.side)
            .ok_or_else(|| AdapterError::InvalidRequest(format!("unknown side '{}' - Metis markets are yes/no", intent.side)))?;
        let wallet = parse_key("wallet", &intent.wallet)?;
        let deposit = to_atoms(intent.amount_usdc)
            .filter(|a| *a > 0)
            .ok_or_else(|| AdapterError::InvalidRequest("amount_usdc must be a positive amount with at most 6 decimal places".to_string()))?;

        let (market_address, market, _) = self.load_market(&intent.venue_market_id).await?;
        let now = now_unix();
        if secondary_open(&market, now) {
            return self.build_buy_on_the_books(intent, side, &wallet, deposit, &market_address, &market).await;
        }
        if !primary_open(&market, now) {
            return Err(AdapterError::InvalidRequest("this market is no longer trading".to_string()));
        }

        // The program's own limits, checked here first so a user gets a plain message instead of
        // a failed transaction. The program still enforces every one of them.
        let quote = quote_side(&market, side, deposit)
            .ok_or_else(|| AdapterError::InvalidRequest("that amount is too small to buy any shares".to_string()))?;
        let dials = self.dials().await?;
        let depth = priced_shares(&market.outcome_shares, market.virtual_per_side).map(|p| cost(&p)).unwrap_or(0);
        let cap = depth * dials.max_in_bps_of_vault as u128 / 10_000;
        if quote.net as u128 > cap {
            return Err(AdapterError::InvalidRequest(format!(
                "the largest single bet on this market right now is about ${} - it grows as the market does",
                usdc((cap.min(u64::MAX as u128)) as u64).round_dp(2)
            )));
        }
        if quote.curve_impact_bps > MAX_PRICE_IMPACT_BPS as u32 {
            return Err(AdapterError::InvalidRequest("that bet would move the market's price too far in one go; bet less".to_string()));
        }

        // The caller states a floor in the quote's own unit (what the bet pays if it wins). The
        // program guards in shares, so apply the same fraction to the shares this quote buys.
        // This bounds how far the curve may move before the bet fills; what the shares finally pay
        // also depends on later bets, which is the nature of a pari-mutuel.
        let (min_shares_out, max_slippage_bps) = match intent.min_shares_out {
            Some(floor) if quote.payout_if_win > 0 => {
                let promised = usdc(quote.payout_if_win);
                let ratio = (floor / promised).clamp(Decimal::ZERO, Decimal::ONE);
                let min = (Decimal::from(quote.shares) * ratio).floor();
                (u64::try_from(min).unwrap_or(0), Some(((Decimal::ONE - ratio) * Decimal::from(10_000u32)).round().try_into().unwrap_or(0i64)))
            }
            _ => (0, None),
        };

        let quote_mint = Pubkey::new_from_array(market.quote_mint.to_bytes());
        let vault = Pubkey::new_from_array(market.vault.to_bytes());
        let mint = outcome_mint(&self.settings.program_id, &market_address, side);
        let args = PlaceBetArgs { deposit_amount: deposit, min_shares_out, outcome_index: side as u8, _padding: [0; 7] };
        let mut data = PLACE_BET.to_vec();
        data.extend_from_slice(bytes_of(&args));
        let place_bet = Instruction {
            program_id: key(&self.settings.program_id),
            accounts: vec![
                meta(&market_address, false, true),
                meta(&vault, false, true),
                meta(&wallet, true, true),
                meta(&associated_token_account(&wallet, &quote_mint), false, true),
                meta(&quote_mint, false, false),
                meta(&mint, false, true),
                meta(&associated_token_account(&wallet, &mint), false, true),
                meta(&self.config_address(), false, false),
                meta(&Pubkey::from_str_const(SYSTEM_PROGRAM), false, false),
                meta(&Pubkey::from_str_const(TOKEN_PROGRAM), false, false),
                meta(&Pubkey::from_str_const(ASSOCIATED_TOKEN_PROGRAM), false, false),
            ],
            data,
        };

        let (transaction, last_valid_block_height) = self.finish(&wallet, vec![place_bet]).await?;
        let payout = usdc(quote.payout_if_win);
        Ok(UnsignedTransaction {
            venue_id: "metis".to_string(),
            kind: IntentKind::BuyShares,
            transaction,
            venue_ref: action_ref("buy", &market_address, &intent.wallet),
            expires_at: now + ASSUMED_BLOCKHASH_SECS,
            required_signers: vec![intent.wallet.clone()],
            last_valid_block_height: Some(last_valid_block_height),
            cost: TransactionCost {
                total_usdc: intent.amount_usdc,
                lines: vec![
                    CostLine { label: "Bet".to_string(), amount_usdc: usdc(quote.net), note: Some("goes into the pot that winners share".to_string()) },
                    CostLine { label: "Fee".to_string(), amount_usdc: usdc(quote.fee), note: Some("taken before the bet, fixed when the market was created".to_string()) },
                ],
            },
            preview: Some(OrderPreview {
                // In the quote's unit: what this bet pays if it wins, so size times price is the stake.
                contracts: Some(payout.to_string()),
                avg_price_usdc: Some((usdc(deposit) / payout).round_dp(6)),
                payout_if_win_usdc: Some(payout),
                slippage_bps: None,
                max_slippage_bps,
            }),
        })
    }

    pub(super) async fn build_claim_transaction(&self, intent: &ClaimIntent) -> Result<UnsignedTransaction, AdapterError> {
        let wallet = parse_key("wallet", &intent.wallet)?;
        let (owner, target) = parse_claim_ref(&intent.venue_ref)
            .ok_or_else(|| AdapterError::InvalidRequest("venue_ref is not a claim reference (claim:<wallet>:<market>)".to_string()))?;
        if owner != intent.wallet {
            return Err(AdapterError::InvalidRequest("that claim belongs to a different wallet".to_string()));
        }
        let market_address = parse_key("market", target)?;
        let data = self.rpc.account(&market_address).await?.ok_or_else(|| AdapterError::MarketNotFound(target.to_string()))?;
        let market = decode_market(&data).ok_or_else(|| AdapterError::MarketNotFound(target.to_string()))?;

        let quote_mint = Pubkey::new_from_array(market.quote_mint.to_bytes());
        let vault = Pubkey::new_from_array(market.vault.to_bytes());
        let wallet_usdc = associated_token_account(&wallet, &quote_mint);
        let program = key(&self.settings.program_id);

        // The wallet's USDC account must exist for a payout to land in it. Creating it when
        // missing is harmless and idempotent.
        let ensure_usdc = create_ata_idempotent(&wallet, &wallet_usdc, &wallet, &quote_mint);
        let token_program = Pubkey::from_str_const(TOKEN_PROGRAM);
        let holder_ix = |discriminator: &[u8; 8], side: usize| {
            let mint = outcome_mint(&self.settings.program_id, &market_address, side);
            Instruction {
                program_id: program,
                accounts: vec![
                    meta(&market_address, false, false),
                    meta(&vault, false, true),
                    meta(&wallet, true, false),
                    meta(&wallet_usdc, false, true),
                    meta(&quote_mint, false, false),
                    meta(&mint, false, true),
                    meta(&associated_token_account(&wallet, &mint), false, true),
                    meta(&token_program, false, false),
                ],
                data: discriminator.to_vec(),
            }
        };

        // A crowd-seeded market's backers hold the raise's token, and redeem it for their slice of the
        // escrowed seed shares. `claim_backer` burns all of it, so one instruction is enough.
        let backer_mint = Pubkey::new_from_array(market.backer_mint.to_bytes());
        let backer_tokens = if market.seed_source == SeedSource::DbcBackers as u8 && backer_mint != Pubkey::default() {
            self.rpc.account(&associated_token_account(&wallet, &backer_mint)).await?.map(|d| token_amount(&d)).unwrap_or(0)
        } else {
            0
        };
        let backer_ix = || {
            let (m0, m1) = (outcome_mint(&self.settings.program_id, &market_address, 0), outcome_mint(&self.settings.program_id, &market_address, 1));
            Instruction {
                program_id: program,
                accounts: vec![
                    meta(&market_address, false, true),
                    meta(&vault, false, true),
                    meta(&wallet, true, false),
                    meta(&wallet_usdc, false, true),
                    meta(&quote_mint, false, false),
                    meta(&backer_mint, false, true),
                    meta(&associated_token_account(&wallet, &backer_mint), false, true),
                    meta(&m0, false, true),
                    meta(&m1, false, true),
                    meta(&associated_token_account(&market_address, &m0), false, true),
                    meta(&associated_token_account(&market_address, &m1), false, true),
                    meta(&token_program, false, false),
                ],
                data: CLAIM_BACKER.to_vec(),
            }
        };

        let status = MarketStatus::try_from(market.status).ok();
        let (mut instructions, kind, payout, label) = match (intent.kind, status) {
            (IntentKind::ClaimWinnings, Some(MarketStatus::Settled)) => {
                let side = market.winning_outcome as usize;
                let held = self.holding(&wallet, &market_address, side).await? + self.seat_slices(&wallet, &market_address, &quote_mint).await?[side];
                if held == 0 && backer_tokens == 0 {
                    return Err(AdapterError::InvalidRequest("this wallet holds no winning shares in this market".to_string()));
                }
                let payout = if held > 0 { payout_atoms(&market, side, held).unwrap_or(0) } else { 0 };
                let mut ixs = Vec::new();
                if held > 0 {
                    ixs.push(holder_ix(&CLAIM, side));
                }
                if backer_tokens > 0 {
                    ixs.push(backer_ix());
                }
                (ixs, IntentKind::ClaimWinnings, payout, "Winnings")
            }
            (IntentKind::ClaimWinnings, Some(MarketStatus::Cancelled)) => {
                // The market was cancelled: every share, on either side, is refunded.
                let mut ixs = Vec::new();
                let mut payout = 0u64;
                let seat = self.seat_slices(&wallet, &market_address, &quote_mint).await?;
                for side in [ID_YES, ID_NO] {
                    let held = self.holding(&wallet, &market_address, side).await? + seat[side];
                    if held > 0 {
                        payout += payout_atoms(&market, side, held).unwrap_or(0);
                        ixs.push(holder_ix(&CLAIM_REFUND, side));
                    }
                }
                if backer_tokens > 0 {
                    ixs.push(backer_ix());
                }
                if ixs.is_empty() {
                    return Err(AdapterError::InvalidRequest("this wallet holds no shares in this cancelled market".to_string()));
                }
                (ixs, IntentKind::ClaimWinnings, payout, "Refund")
            }
            (IntentKind::ClaimWinnings, _) => {
                return Err(AdapterError::InvalidRequest("this market has not been settled or cancelled yet, so there is nothing to claim".to_string()));
            }
            (IntentKind::ClaimCreatorFees, Some(MarketStatus::Settled)) => {
                if Pubkey::new_from_array(market.creator.to_bytes()) != wallet {
                    return Err(AdapterError::InvalidRequest("only the market's creator can claim its fees".to_string()));
                }
                if market.creator_owed == 0 {
                    return Err(AdapterError::InvalidRequest("there are no creator fees left to claim on this market".to_string()));
                }
                let sweep = Instruction {
                    program_id: program,
                    accounts: vec![
                        meta(&market_address, false, true),
                        meta(&vault, false, true),
                        meta(&wallet_usdc, false, true),
                        meta(&quote_mint, false, false),
                        meta(&self.config_address(), false, false),
                        meta(&token_program, false, false),
                        // The creator's own sweep needs nobody's permission, only a signer (the
                        // creator) so the program's account layout matches the protocol sweep's.
                        meta(&wallet, true, false),
                    ],
                    data: [SWEEP_FEES.as_slice(), bytes_of(&SweepFeesArgs { which: SWEEP_CREATOR, _padding: [0; 7] })].concat(),
                };
                (vec![sweep], IntentKind::ClaimCreatorFees, market.creator_owed, "Creator fees")
            }
            (IntentKind::ClaimCreatorFees, _) => {
                return Err(AdapterError::InvalidRequest("a market's creator fees can be claimed once it has settled".to_string()));
            }
            _ => return Err(AdapterError::InvalidRequest("Metis can claim winnings or creator fees".to_string())),
        };
        // A maker's seat balances come home first, in the same transaction (the claim then burns
        // whatever the wallet holds, withdrawn slices included).
        if matches!(kind, IntentKind::ClaimWinnings) {
            let sides: Vec<usize> = match status {
                Some(MarketStatus::Settled) => vec![market.winning_outcome as usize],
                _ => vec![ID_YES, ID_NO],
            };
            let withdrawals = self.seat_withdrawals(&wallet, &market_address, &quote_mint, &sides).await?;
            instructions.splice(0..0, withdrawals);
        }
        instructions.insert(0, ensure_usdc);

        let (transaction, last_valid_block_height) = self.finish(&wallet, instructions).await?;
        let payout_usdc = usdc(payout);
        Ok(UnsignedTransaction {
            venue_id: "metis".to_string(),
            kind,
            transaction,
            venue_ref: action_ref("claim", &market_address, &intent.wallet),
            expires_at: now_unix() + ASSUMED_BLOCKHASH_SECS,
            required_signers: vec![intent.wallet.clone()],
            last_valid_block_height: Some(last_valid_block_height),
            // Claiming costs the signer nothing but the network fee.
            cost: TransactionCost { total_usdc: Decimal::ZERO, lines: Vec::new() },
            preview: Some(OrderPreview { contracts: None, avg_price_usdc: None, payout_if_win_usdc: Some(payout_usdc), slippage_bps: None, max_slippage_bps: None })
                .filter(|_| !label.is_empty() && payout > 0),
        })
    }

    /// Buys slices from the order book: one Manifest swap, spending exactly the amount asked and
    /// taking whatever the book gives for it, never less than the floor.
    async fn build_buy_on_the_books(
        &self,
        intent: &BuyIntent,
        side: usize,
        wallet: &Pubkey,
        deposit: u64,
        market_address: &Pubkey,
        market: &metis_core::amm::state::Market,
    ) -> Result<UnsignedTransaction, AdapterError> {
        let books = self.books_of(&[(*market_address, *market)]).await?;
        let depth = books
            .get(market_address)
            .and_then(|b| b[side].as_ref())
            .ok_or_else(|| AdapterError::InvalidRequest("this market's order book has not been opened yet".to_string()))?;
        if !depth.tradable() {
            return Err(AdapterError::InvalidRequest("this order book holds order types Metis cannot route yet".to_string()));
        }
        let fill = buy_with(depth, deposit);
        if fill.base_atoms == 0 {
            return Err(AdapterError::NoLiquidity { venue_market_id: intent.venue_market_id.clone(), side: side_name(side).to_string() });
        }
        // After betting closes the pot is final, so what these slices pay if they win is exact.
        let payout = payout_atoms(market, side, fill.base_atoms).unwrap_or(0);
        let payout_dec = usdc(payout);

        // The caller's floor is in payout terms; the swap guards in slices. Same fraction.
        let (min_out, max_slippage_bps) = match intent.min_shares_out {
            Some(floor) if payout > 0 => {
                let ratio = (floor / payout_dec).clamp(Decimal::ZERO, Decimal::ONE);
                let min = (Decimal::from(fill.base_atoms) * ratio).floor();
                (u64::try_from(min).unwrap_or(0), Some(((Decimal::ONE - ratio) * Decimal::from(10_000u32)).round().try_into().unwrap_or(0i64)))
            }
            _ => (0, None),
        };

        let quote_mint = Pubkey::new_from_array(market.quote_mint.to_bytes());
        let mint = outcome_mint(&self.settings.program_id, market_address, side);
        let slice_ata = associated_token_account(wallet, &mint);
        let usdc_ata = associated_token_account(wallet, &quote_mint);
        let swap = from_manifest(swap_instruction(
            manifest_pubkey(wallet),
            manifest_pubkey(&self.book_address(market_address, side)),
            manifest_pubkey(&slice_ata),
            manifest_pubkey(&usdc_ata),
            manifest_pubkey(&mint),
            manifest_pubkey(&quote_mint),
            manifest_pubkey(&Pubkey::from_str_const(TOKEN_PROGRAM)),
            None,
            false,
            false,
            SwapParams::new(deposit, min_out, false, true),
            false,
        ));
        // The slices land in the wallet's own token account, which may not exist yet.
        let instructions = vec![create_ata_idempotent(wallet, &slice_ata, wallet, &mint), swap];

        let (transaction, last_valid_block_height) = self.finish(wallet, instructions).await?;
        Ok(UnsignedTransaction {
            venue_id: "metis".to_string(),
            kind: IntentKind::BuyShares,
            transaction,
            venue_ref: action_ref("buy", market_address, &intent.wallet),
            expires_at: now_unix() + ASSUMED_BLOCKHASH_SECS,
            required_signers: vec![intent.wallet.clone()],
            last_valid_block_height: Some(last_valid_block_height),
            cost: TransactionCost {
                total_usdc: usdc(fill.quote_atoms),
                lines: vec![CostLine {
                    label: "Bought on the order book".to_string(),
                    amount_usdc: usdc(fill.quote_atoms),
                    note: Some(if fill.complete {
                        "from holders selling at these prices; there is no fee".to_string()
                    } else {
                        "the book is thinner than the amount asked for, so only this much will be spent".to_string()
                    }),
                }],
            },
            preview: Some(OrderPreview {
                contracts: Some(payout_dec.to_string()),
                avg_price_usdc: (payout > 0).then(|| (usdc(fill.quote_atoms) / payout_dec).round_dp(6)),
                payout_if_win_usdc: Some(payout_dec),
                slippage_bps: None,
                max_slippage_bps,
            }),
        })
    }

    /// Sells a whole position on the order book: one Manifest swap, taking the best bids and never
    /// accepting less than the quoted proceeds minus a fixed margin.
    pub(super) async fn build_sell_transaction(&self, intent: &SellIntent) -> Result<UnsignedTransaction, AdapterError> {
        let wallet = parse_key("wallet", &intent.wallet)?;
        let (market_address, side) = parse_position_ref(&intent.venue_ref)
            .ok_or_else(|| AdapterError::InvalidRequest("venue_ref is not a Metis position (<market>:<yes|no>)".to_string()))?;
        let data = self.rpc.account(&market_address).await?.ok_or_else(|| AdapterError::MarketNotFound(market_address.to_string()))?;
        let market = decode_market(&data).ok_or_else(|| AdapterError::MarketNotFound(market_address.to_string()))?;
        if !secondary_open(&market, now_unix()) {
            return Err(AdapterError::InvalidRequest(
                "this market isn't trading on the order book: before betting closes there is nothing to sell into, and once it has settled you claim instead".to_string(),
            ));
        }

        let held = self.holding(&wallet, &market_address, side).await?;
        if held == 0 {
            return Err(AdapterError::InvalidRequest("this wallet holds no shares of that side".to_string()));
        }
        let books = self.books_of(&[(market_address, market)]).await?;
        let depth = books
            .get(&market_address)
            .and_then(|b| b[side].as_ref())
            .ok_or_else(|| AdapterError::InvalidRequest("this market's order book has not been opened yet".to_string()))?;
        if !depth.tradable() {
            return Err(AdapterError::InvalidRequest("this order book holds order types Metis cannot route yet".to_string()));
        }
        let fill = sell_into(depth, held);
        if fill.base_atoms == 0 || fill.quote_atoms == 0 {
            return Err(AdapterError::NoLiquidity { venue_market_id: market_address.to_string(), side: side_name(side).to_string() });
        }
        let min_out = (fill.quote_atoms as u128 * (10_000 - SELL_SLIPPAGE_BPS as u128) / 10_000) as u64;

        let quote_mint = Pubkey::new_from_array(market.quote_mint.to_bytes());
        let mint = outcome_mint(&self.settings.program_id, &market_address, side);
        let slice_ata = associated_token_account(&wallet, &mint);
        let usdc_ata = associated_token_account(&wallet, &quote_mint);
        let swap = from_manifest(swap_instruction(
            manifest_pubkey(&wallet),
            manifest_pubkey(&self.book_address(&market_address, side)),
            manifest_pubkey(&slice_ata),
            manifest_pubkey(&usdc_ata),
            manifest_pubkey(&mint),
            manifest_pubkey(&quote_mint),
            manifest_pubkey(&Pubkey::from_str_const(TOKEN_PROGRAM)),
            None,
            false,
            false,
            // Sell `held` slices, exactly, for at least `min_out` USDC.
            SwapParams::new(held, min_out, true, true),
            false,
        ));
        // The proceeds land in the wallet's USDC account, which a seller may not have.
        let instructions = vec![create_ata_idempotent(&wallet, &usdc_ata, &wallet, &quote_mint), swap];

        let (transaction, last_valid_block_height) = self.finish(&wallet, instructions).await?;
        let sold = usdc(fill.base_atoms);
        Ok(UnsignedTransaction {
            venue_id: "metis".to_string(),
            kind: IntentKind::SellShares,
            transaction,
            venue_ref: action_ref("sell", &market_address, &intent.wallet),
            expires_at: now_unix() + ASSUMED_BLOCKHASH_SECS,
            required_signers: vec![intent.wallet.clone()],
            last_valid_block_height: Some(last_valid_block_height),
            // Selling costs nothing but the network fee.
            cost: TransactionCost { total_usdc: Decimal::ZERO, lines: Vec::new() },
            preview: Some(OrderPreview {
                contracts: Some(sold.to_string()),
                // What each slice sells for on average, in USDC.
                avg_price_usdc: Some((usdc(fill.quote_atoms) / sold).round_dp(6)),
                payout_if_win_usdc: payout_atoms(&market, side, fill.base_atoms).map(usdc),
                slippage_bps: None,
                // The venue's own margin, not one the caller chose.
                max_slippage_bps: Some(SELL_SLIPPAGE_BPS),
            }),
        })
    }

    /// Slices of each side the wallet has on the books: parked in its seat, or offered in resting
    /// asks (a claim cancels those orders first, which returns the slices to the seat).
    async fn seat_slices(&self, wallet: &Pubkey, market_address: &Pubkey, quote_mint: &Pubkey) -> Result<[u64; 2], AdapterError> {
        let books: Vec<Pubkey> = [ID_YES, ID_NO].iter().map(|s| self.book_address(market_address, *s)).collect();
        let accounts = self.rpc.accounts(&books).await?;
        let mut out = [0u64; 2];
        for side in [ID_YES, ID_NO] {
            let Some(Some(data)) = accounts.get(side) else { continue };
            let shares = outcome_mint(&self.settings.program_id, market_address, side);
            out[side] = seat_holding(data, wallet, &shares, quote_mint).map_or(0, |s| s.slices());
        }
        Ok(out)
    }

    /// The Manifest withdrawals that bring whatever a maker has parked in its seats back to its own
    /// token accounts, so one claim also collects it: slices (of `sides`) and USDC proceeds. Slices
    /// still offered in resting orders can't be withdrawn until those orders are cancelled, so they
    /// are not included. Empty for a wallet with no seat.
    async fn seat_withdrawals(
        &self,
        wallet: &Pubkey,
        market_address: &Pubkey,
        quote_mint: &Pubkey,
        sides: &[usize],
    ) -> Result<Vec<Instruction>, AdapterError> {
        let books: Vec<Pubkey> = [ID_YES, ID_NO].iter().map(|s| self.book_address(market_address, *s)).collect();
        let accounts = self.rpc.accounts(&books).await?;
        let token = manifest_pubkey(&Pubkey::from_str_const(TOKEN_PROGRAM));
        let mut ixs = Vec::new();
        let mut quote_ixs = Vec::new();
        for side in [ID_YES, ID_NO] {
            let Some(Some(data)) = accounts.get(side) else { continue };
            let shares = outcome_mint(&self.settings.program_id, market_address, side);
            let Some(seat) = seat_holding(data, wallet, &shares, quote_mint) else { continue };
            let book = manifest_pubkey(&books[side]);
            // Orders still resting lock slices (asks) or USDC (bids); cancelling them returns both
            // to the seat, where they can be withdrawn.
            if !seat.orders.is_empty() {
                let mut batch = manifest_client::BatchUpdateParams::new();
                for (sequence, at) in &seat.orders {
                    batch = batch.add_cancel(manifest_client::CancelOrderParams::with_hint(*sequence, *at));
                }
                ixs.push(from_manifest(manifest_client::batch_update_instruction(manifest_pubkey(wallet), book, batch)));
            }
            let base = seat.base_withdrawable.saturating_add(seat.resting_ask_base);
            if base > 0 && sides.contains(&side) {
                let ata = associated_token_account(wallet, &shares);
                ixs.push(create_ata_idempotent(wallet, &ata, wallet, &shares));
                ixs.push(from_manifest(withdraw_instruction(
                    manifest_pubkey(wallet),
                    book,
                    manifest_pubkey(&ata),
                    manifest_pubkey(&shares),
                    token,
                    WithdrawParams::new(base),
                )));
            }
            if seat.quote_withdrawable > 0 {
                let ata = associated_token_account(wallet, quote_mint);
                quote_ixs.push(from_manifest(withdraw_instruction(
                    manifest_pubkey(wallet),
                    book,
                    manifest_pubkey(&ata),
                    manifest_pubkey(quote_mint),
                    token,
                    WithdrawParams::new(seat.quote_withdrawable),
                )));
            }
        }
        ixs.extend(quote_ixs);
        Ok(ixs)
    }

    /// How many shares of one outcome the wallet holds, straight from its token account.
    async fn holding(&self, wallet: &Pubkey, market: &Pubkey, side: usize) -> Result<u64, AdapterError> {
        let mint = outcome_mint(&self.settings.program_id, market, side);
        let account = associated_token_account(wallet, &mint);
        Ok(self.rpc.account(&account).await?.map(|d| token_amount(&d)).unwrap_or(0))
    }
}

fn manifest_pubkey(pubkey: &Pubkey) -> manifest_client::Pubkey {
    manifest_client::Pubkey::new_from_array(pubkey.to_bytes())
}

// Silences the unused import when QUOTE_DECIMALS isn't needed by a build configuration.
const _: u8 = QUOTE_DECIMALS;
