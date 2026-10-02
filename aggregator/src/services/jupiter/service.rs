//! Jupiter Prediction: aggregates Polymarket + Kalshi liquidity behind one API, settled on
//! Solana. Order-book based (unlike Panta's bonding curve), YES/NO framed, user-signed-per-trade
//! (POST /orders returns an unsigned transaction the caller signs and submits via POST /execute -
//! no delegated trading key here). Requires an `x-api-key` header, per Jupiter's own OpenAPI spec
//! - confirmed this session, correcting an earlier assumption that this venue needed no key.
//!
//! Business-logic half of the integration - the `VenueAdapter` impl, including the trading
//! decision of which book side to walk. Raw HTTP/wire calls live in `client.rs`; the book-walking
//! math itself lives in `services::maths::orderbook`.

use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::execution::{BuyIntent, ClaimIntent, ConfirmOutcome, SellIntent, UnsignedTransaction};
use crate::models::position::Position;
use crate::models::venue::{
    ListParams, MarketPage, OutcomeFraming, TradingModel, VenueCapabilities, VenueMarket, VenueQuote,
};
use crate::services::VenueAdapter;
use crate::services::maths::orderbook::{Level, walk_book};
use crate::utils::http::client_with_timeout;

use super::{client, execution, positions};

/// Talks to Jupiter Prediction's REST API on behalf of `VenueAdapter`. Base URL and API key both
/// come from `Config`, passed in at construction.
pub struct JupiterAdapter {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    /// Programs a built transaction may invoke beyond the defaults (`JUPITER_EXTRA_PROGRAMS`).
    extra_programs: Vec<String>,
}

/// Jupiter Prediction's issuer program, from its developer documentation
/// (<https://developers.jup.ag/docs/prediction/forecast>). Every order, sale and claim
/// transaction it builds goes through this program.
pub const ISSUER_PROGRAM: &str = "2sVcg2dBSUzXkmdZ8M5cp1LbnzDrWJmr6hktkHwB8nY3";

impl JupiterAdapter {
    /// Builds an adapter for the given base URL and API key.
    pub fn new(base_url: String, api_key: String) -> Self {
        Self { client: client_with_timeout(), base_url, api_key, extra_programs: Vec::new() }
    }

    /// Allows further programs in built transactions, for a program Jupiter adds that this
    /// service has not been told about yet. Blank entries are ignored.
    pub fn with_extra_programs(mut self, programs: impl IntoIterator<Item = String>) -> Self {
        self.extra_programs = programs.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
        self
    }

    /// Refuses a transaction Jupiter built unless it is a plain trade for `wallet`: see
    /// `solana_tx::verify_venue_transaction`. Runs on every buy, sell and claim before the
    /// transaction leaves this service.
    fn guard(&self, built: UnsignedTransaction, wallet: &str) -> Result<UnsignedTransaction, AdapterError> {
        let mut allowed: Vec<&str> = vec![ISSUER_PROGRAM];
        allowed.extend(crate::utils::solana_tx::COMMON_PROGRAMS);
        allowed.extend(self.extra_programs.iter().map(String::as_str));
        crate::utils::solana_tx::verify_venue_transaction(&built.transaction, wallet, &allowed).map_err(|e| {
            // Refused, not failed: the venue answered, and what it answered is not safe to sign.
            // Logged loudly because a tampered response and a program Jupiter newly started using
            // look identical from here, and an operator has to tell which.
            tracing::error!("jupiter: refused to pass on a built transaction for {wallet}: {e}");
            AdapterError::Venue(format!("jupiter: {e}"))
        })?;
        Ok(built)
    }
}

/// Parses raw (price, quantity) pairs into `Level`s, skipping bad rows. Price is a decimal
/// string; quantity is a raw JSON number (unlike every other venue's book, which sends both as
/// strings) - converted via `from_f64_retain` rather than a plain cast, so a row with a
/// NaN/infinite quantity gets skipped rather than silently becoming zero.
fn parse_levels(raw: &[(String, f64)]) -> Vec<Level> {
    raw.iter()
        .filter_map(|(price, size)| {
            let price: Decimal = price.parse().ok()?;
            let size = Decimal::from_f64_retain(*size)?;
            Some(Level { price, size })
        })
        .collect()
}

#[async_trait]
impl VenueAdapter for JupiterAdapter {
    fn id(&self) -> &'static str {
        "jupiter"
    }

    fn capabilities(&self) -> VenueCapabilities {
        VenueCapabilities {
            framing: OutcomeFraming::YesNo,
            trading_model: TradingModel::UserSignedPerTrade,
            has_order_book: true,
            supports_category_filter: true,
            cursor_pagination_advances: true,
            // POST /orders builds a buy transaction for the caller to sign.
            can_execute: true,
            // Jupiter surfaces Polymarket/Kalshi markets; it has no create API.
            can_create_markets: false,
            can_list_positions: true,
            can_claim: true,
            can_sell: true,
        }
    }

    /// Jupiter has no categories endpoint, so its filter vocabulary is the one piece of venue
    /// knowledge kept in code (`FILTER_CATEGORIES`). The refresh's unfiltered sweep still picks
    /// up any category it adds.
    async fn filter_categories(&self) -> Result<Vec<String>, AdapterError> {
        Ok(crate::models::venue::jupiter::FILTER_CATEGORIES.iter().map(|c| c.to_string()).collect())
    }

    async fn list_markets(&self, params: ListParams) -> Result<MarketPage, AdapterError> {
        client::list_markets(&self.client, &self.base_url, &self.api_key, params).await
    }

    async fn get_market(&self, venue_market_id: &str) -> Result<VenueMarket, AdapterError> {
        client::get_market(&self.client, &self.base_url, &self.api_key, venue_market_id).await
    }

    fn raw_path_prefixes(&self) -> &'static [&'static str] {
        &["events", "markets", "orderbook"]
    }

    async fn raw_get(&self, path: &str, query: &[(String, String)]) -> Result<(u16, String), AdapterError> {
        client::raw_get(&self.client, &self.base_url, &self.api_key, path, query).await
    }

    async fn build_buy(&self, intent: &BuyIntent) -> Result<UnsignedTransaction, AdapterError> {
        let built = execution::build_buy(&self.client, &self.base_url, &self.api_key, intent).await?;
        self.guard(built, &intent.wallet)
    }

    async fn build_claim(&self, intent: &ClaimIntent) -> Result<UnsignedTransaction, AdapterError> {
        let built = positions::build_claim(&self.client, &self.base_url, &self.api_key, intent).await?;
        self.guard(built, &intent.wallet)
    }

    async fn build_sell(&self, intent: &SellIntent) -> Result<UnsignedTransaction, AdapterError> {
        let built = positions::build_sell(&self.client, &self.base_url, &self.api_key, intent).await?;
        self.guard(built, &intent.wallet)
    }

    async fn positions(&self, wallet: &str) -> Result<Vec<Position>, AdapterError> {
        positions::list_positions(&self.client, &self.base_url, &self.api_key, wallet).await
    }

    async fn status(&self, venue_ref: &str) -> Result<ConfirmOutcome, AdapterError> {
        // A claim has no order of its own; its reference says so and its status is the position's.
        if crate::models::execution::parse_claim_ref(venue_ref).is_some() {
            return positions::claim_status(&self.client, &self.base_url, &self.api_key, venue_ref).await;
        }
        execution::order_status(&self.client, &self.base_url, &self.api_key, venue_ref).await
    }

    async fn confirm(
        &self,
        venue_ref: &str,
        _signature: &str,
    ) -> Result<ConfirmOutcome, AdapterError> {
        // Jupiter tracks an order by its own `orderPubkey`, which it minted when the order was
        // built, so it needs nothing from us to look one up. The signature is accepted for
        // interface symmetry and ignored - which also makes this identical to status polling, and
        // therefore safe to call as often as a caller likes.
        self.status(venue_ref).await
    }

    async fn quote(
        &self,
        venue_market_id: &str,
        side: &str,
        amount_usdc: Decimal,
    ) -> Result<VenueQuote, AdapterError> {
        let book = client::get_orderbook(&self.client, &self.base_url, &self.api_key, venue_market_id).await?;

        // Jupiter's `yes_dollars`/`no_dollars` are **bid ladders ascending by price**, not asks -
        // so buying a side means crossing the *opposite* side's bids at the complementary price.
        // Verified arithmetically against both markets of one event (POLY-4350026):
        //
        //   Oregon         catalog buy-yes 0.999 == 1 - best_no_bid (1 - 0.0010)
        //   Portland State catalog buy-yes 0.002 == 1 - best_no_bid (1 - 0.9980)
        //
        // and the two markets' ladders are mirror images of each other, as complementary outcomes
        // of the same event must be. Walking the same-side ladder from index 0 instead (its
        // *lowest* bid) is what made every quote come back at ~0.001 regardless of the real price.
        //
        // Hence: reverse to put the best bid first, then invert each price to the cost of buying.
        let opposite_side_bids = match side {
            "yes" => &book.no_dollars,
            "no" => &book.yes_dollars,
            other => return Err(AdapterError::Venue(format!("unknown side '{other}' for jupiter"))),
        };
        let levels: Vec<Level> = parse_levels(opposite_side_bids)
            .into_iter()
            .rev()
            .map(|level| Level { price: Decimal::ONE - level.price, size: level.size })
            .collect();

        let fill = walk_book(&levels, amount_usdc);
        if fill.filled_size == Decimal::ZERO {
            return Err(AdapterError::NoLiquidity {
                venue_market_id: venue_market_id.to_string(),
                side: side.to_string(),
            });
        }

        Ok(VenueQuote {
            venue: "jupiter",
            venue_market_id: venue_market_id.to_string(),
            side: side.to_string(),
            amount_usdc,
            effective_shares_or_size: fill.filled_size.round_dp(6),
            avg_price: fill.avg_price.round_dp(6),
            // The book walk already knows whether the whole amount filled - carry its answer
            // rather than letting a caller re-derive it from these rounded values.
            fully_filled: fill.fully_filled,
            // Jupiter's fee is dynamic (scales with price + size + uncertainty) and only known
            // once an order is actually created via POST /orders - not derivable from the book
            // alone, so this is left unestimated rather than guessed. Filled in for real once
            // Phase 5 (execution) actually calls /orders.
            fee_usdc: Decimal::ZERO,
            expires_at: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_order_book_yes_no_user_signed() {
        let caps = JupiterAdapter::new("https://api.jup.ag/prediction/v1".to_string(), "test-key".to_string())
            .capabilities();
        assert_eq!(caps.framing, OutcomeFraming::YesNo);
        assert_eq!(caps.trading_model, TradingModel::UserSignedPerTrade);
        assert!(caps.has_order_book);
    }

    /// The real POLY-4350026 book, reduced to the levels that matter: two complementary markets
    /// whose catalog buy-yes prices were 0.002 and 0.999.
    #[test]
    fn buying_a_side_crosses_the_opposite_bid_ladder_at_the_complementary_price() {
        // Portland State: its NO ladder tops out at 0.9980, so buying YES must cost ~0.002.
        let no_bids = vec![("0.0010".to_string(), 259990.0), ("0.9980".to_string(), 468965.75)];
        let levels: Vec<Level> = parse_levels(&no_bids)
            .into_iter()
            .rev()
            .map(|l| Level { price: Decimal::ONE - l.price, size: l.size })
            .collect();

        // Best level first, priced as the cost of buying - not the raw 0.0010 bid.
        assert_eq!(levels[0].price, "0.002".parse::<Decimal>().unwrap());
        assert_eq!(levels[1].price, "0.999".parse::<Decimal>().unwrap());

        let fill = walk_book(&levels, Decimal::from(50));
        assert!(fill.fully_filled);
        assert_eq!(fill.avg_price, "0.002".parse::<Decimal>().unwrap());
    }

    #[test]
    fn an_empty_opposite_ladder_yields_no_liquidity_not_a_zero_price() {
        // Longshot markets really do have empty ladders; that must not read as a free fill.
        let levels = parse_levels(&[]);
        let fill = walk_book(&levels, Decimal::from(50));
        assert_eq!(fill.filled_size, Decimal::ZERO);
        assert!(!fill.fully_filled);
    }

    #[test]
    fn parse_levels_skips_unparseable_or_non_finite_rows() {
        let raw = vec![
            ("0.5".to_string(), 100.0),
            ("not-a-number".to_string(), 100.0),
            ("0.6".to_string(), f64::NAN),
        ];
        let levels = parse_levels(&raw);
        assert_eq!(levels.len(), 1);
    }

    // ── The transaction guard, as the adapter applies it ──────────────────────────────────────

    use crate::models::execution::{IntentKind, TransactionCost};
    use crate::utils::solana_tx::{self, AccountMeta, Instruction};

    fn key(n: u8) -> solana_tx::Pubkey {
        let mut k = [0u8; 32];
        k[0] = n;
        k[31] = n.wrapping_mul(7).wrapping_add(1);
        k
    }

    fn built(instructions: &[Instruction]) -> UnsignedTransaction {
        use base64::Engine as _;
        let bytes = solana_tx::compile_unsigned_v0(key(1), instructions, [9; 32]).unwrap();
        UnsignedTransaction {
            venue_id: "jupiter".to_string(),
            kind: IntentKind::BuyShares,
            transaction: base64::engine::general_purpose::STANDARD.encode(bytes),
            venue_ref: "Order1".to_string(),
            expires_at: 0,
            required_signers: vec![],
            last_valid_block_height: None,
            cost: TransactionCost { total_usdc: Decimal::ZERO, lines: vec![] },
            preview: None,
        }
    }

    fn trade_on(program: &str) -> Instruction {
        Instruction {
            program_id: solana_tx::decode_32(program).unwrap(),
            accounts: vec![AccountMeta { pubkey: key(1), is_signer: true, is_writable: true }],
            data: vec![1],
        }
    }

    fn wallet() -> String {
        solana_tx::encode_32(&key(1))
    }

    #[test]
    fn a_trade_through_jupiters_issuer_program_is_passed_on_unchanged() {
        let adapter = JupiterAdapter::new("http://x".into(), "k".into());
        let tx = built(&[trade_on(ISSUER_PROGRAM)]);
        let original = tx.transaction.clone();
        assert_eq!(adapter.guard(tx, &wallet()).unwrap().transaction, original);
    }

    #[test]
    fn a_transaction_invoking_an_unknown_program_is_refused_until_it_is_configured() {
        let rogue = solana_tx::encode_32(&key(200));
        let adapter = JupiterAdapter::new("http://x".into(), "k".into());
        let err = adapter.guard(built(&[trade_on(&rogue)]), &wallet()).unwrap_err();
        assert!(err.to_string().contains("unexpected program"), "{err}");

        let adapter = adapter.with_extra_programs([format!(" {rogue} "), String::new()]);
        assert!(adapter.guard(built(&[trade_on(&rogue)]), &wallet()).is_ok(), "an operator can allow it explicitly");
    }

    #[test]
    fn a_transaction_built_for_another_wallet_is_refused() {
        let adapter = JupiterAdapter::new("http://x".into(), "k".into());
        let other = solana_tx::encode_32(&key(2));
        assert!(adapter.guard(built(&[trade_on(ISSUER_PROGRAM)]), &other).is_err());
    }

    #[test]
    fn the_issuer_program_is_a_valid_key() {
        assert!(solana_tx::decode_32(ISSUER_PROGRAM).is_ok());
    }
}
