//! Pascal: a non-custodial, order-book prediction market exchange on Solana. Structurally the odd
//! one out of the three venues: no YES/NO tokens at all - contracts are bilateral, fully
//! collateralized derivatives settling to a price in [$0, $1], and a trader is either long (gets
//! the settlement price) or short (gets 1 - price). It's also the first venue integrated with a
//! real `DelegatedTradingKey`: a separate, rotatable trading key signs orders, not the wallet
//! itself - this adapter only *quotes* (reads the book), it doesn't place orders, so no signing
//! happens here yet. Wiring an actual trading key in is explicitly deferred to execution
//! (Phase 5+), flagged in the integration plan as needing its own custody design discussion
//! before the router can pick Pascal for a live trade.
//!
//! Business-logic half of the integration - the `VenueAdapter` impl, including the trading
//! decision of which book side to walk and the short-side price inversion. Raw HTTP/wire calls
//! live in `client.rs`; the book-walking math itself lives in `services::maths::orderbook`.

use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::venue::{
    ListParams, MarketPage, OutcomeFraming, TradingModel, VenueCapabilities, VenueMarket, VenueQuote,
};
use crate::services::VenueAdapter;
use crate::services::maths::orderbook::{Level, walk_book};
use crate::utils::http::client_with_timeout;

use super::client;

/// Talks to Pascal's REST API on behalf of `VenueAdapter` - quoting only. Base URL comes from
/// `Config`, passed in at construction - no API key needed for read-only calls.
pub struct PascalAdapter {
    client: reqwest::Client,
    base_url: String,
}

impl PascalAdapter {
    /// Builds an adapter for the given base URL.
    pub fn new(base_url: String) -> Self {
        Self { client: client_with_timeout(), base_url }
    }
}

/// Parses raw (price, size) string tuples into `Level`s, skipping bad rows.
fn parse_levels(raw: &[(String, String)]) -> Vec<Level> {
    raw.iter()
        .filter_map(|(price, size)| {
            let price: Decimal = price.parse().ok()?;
            let size: Decimal = size.parse().ok()?;
            Some(Level { price, size })
        })
        .collect()
}

#[async_trait]
impl VenueAdapter for PascalAdapter {
    fn id(&self) -> &'static str {
        "pascal"
    }

    fn capabilities(&self) -> VenueCapabilities {
        VenueCapabilities {
            framing: OutcomeFraming::LongShort,
            trading_model: TradingModel::DelegatedTradingKey,
            has_order_book: true,
            supports_category_filter: false,
            cursor_pagination_advances: true,
            // Pascal signs orders with a delegated trading key, which needs its own
            // custody design before anything is wired - see the adapter's module doc.
            can_execute: false,
            can_create_markets: false,
        }
    }

    async fn list_markets(&self, _params: ListParams) -> Result<MarketPage, AdapterError> {
        client::list_markets(&self.client, &self.base_url).await
    }

    async fn get_market(&self, venue_market_id: &str) -> Result<VenueMarket, AdapterError> {
        client::get_market(&self.client, &self.base_url, venue_market_id).await
    }

    async fn quote(
        &self,
        venue_market_id: &str,
        side: &str,
        amount_usdc: Decimal,
    ) -> Result<VenueQuote, AdapterError> {
        let book = client::get_book(&self.client, &self.base_url, venue_market_id).await?;

        // "long" buys at the ask (paying up to get long); "short" effectively buys at 1 - bid
        // (going short is economically "selling" the contract) - walked the same way either
        // side, just against the opposite book and, for short, in inverted-price space. This is
        // a trading decision, made here rather than in client.rs.
        let levels = match side {
            "long" => parse_levels(&book.asks),
            "short" => parse_levels(&book.bids)
                .into_iter()
                .map(|level| Level { price: Decimal::ONE - level.price, size: level.size })
                .collect(),
            other => return Err(AdapterError::Venue(format!("unknown side '{other}' for pascal"))),
        };

        let fill = walk_book(&levels, amount_usdc);
        if fill.filled_size == Decimal::ZERO {
            return Err(AdapterError::NoLiquidity {
                venue_market_id: venue_market_id.to_string(),
                side: side.to_string(),
            });
        }

        Ok(VenueQuote {
            venue: "pascal",
            venue_market_id: venue_market_id.to_string(),
            side: side.to_string(),
            amount_usdc,
            effective_shares_or_size: fill.filled_size.round_dp(6),
            avg_price: fill.avg_price.round_dp(6),
            // The book walk already knows whether the whole amount filled - carry its answer
            // rather than letting a caller re-derive it from these rounded values.
            fully_filled: fill.fully_filled,
            // Pascal's real taker fee (2% default, size * price * (1-price) * rate) needs the
            // actual fill to compute precisely - left unestimated here rather than approximated,
            // same call as Jupiter's fee_usdc placeholder, filled in once execution is built.
            fee_usdc: Decimal::ZERO,
            expires_at: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_order_book_long_short_delegated_key() {
        let caps = PascalAdapter::new("https://data.pascal.trade/api/v1".to_string()).capabilities();
        assert_eq!(caps.framing, OutcomeFraming::LongShort);
        assert_eq!(caps.trading_model, TradingModel::DelegatedTradingKey);
        assert!(caps.has_order_book);
    }

    #[test]
    fn short_side_inverts_price_against_the_bid_book() {
        let raw = vec![("0.6".to_string(), "100".to_string())];
        let levels = parse_levels(&raw);
        let inverted: Vec<Level> =
            levels.into_iter().map(|l| Level { price: Decimal::ONE - l.price, size: l.size }).collect();
        assert_eq!(inverted[0].price, "0.4".parse::<Decimal>().unwrap());
    }
}
