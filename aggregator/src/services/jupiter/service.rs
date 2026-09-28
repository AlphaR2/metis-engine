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
use crate::models::execution::{BuyIntent, ConfirmOutcome, UnsignedTransaction};
use crate::models::venue::{
    ListParams, MarketPage, OutcomeFraming, TradingModel, VenueCapabilities, VenueMarket, VenueQuote,
};
use crate::services::VenueAdapter;
use crate::services::maths::orderbook::{Level, walk_book};
use crate::utils::http::client_with_timeout;

use super::{client, execution};

/// Talks to Jupiter Prediction's REST API on behalf of `VenueAdapter`. Base URL and API key both
/// come from `Config`, passed in at construction.
pub struct JupiterAdapter {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl JupiterAdapter {
    /// Builds an adapter for the given base URL and API key.
    pub fn new(base_url: String, api_key: String) -> Self {
        Self { client: client_with_timeout(), base_url, api_key }
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
        execution::build_buy(&self.client, &self.base_url, &self.api_key, intent).await
    }

    async fn status(&self, venue_ref: &str) -> Result<ConfirmOutcome, AdapterError> {
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
        execution::order_status(&self.client, &self.base_url, &self.api_key, venue_ref).await
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
        // This is the same long/short inversion `PascalAdapter` does, arrived at independently.
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
}
