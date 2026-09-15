pub mod panta;
pub mod pascal;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How a venue frames a binary proposition. Panta: literal yes/no tokens. Pascal: no yes/no at
/// all — you're long or short a contract that settles to a price in [$0, $1]. The canonical layer
/// normalizes across this; adapters report their own native framing so nothing is lost translating
/// into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutcomeFraming {
    YesNo,
    LongShort,
}

/// Whether a venue supports unattended trading. This is the concrete answer to "can an AI/bot use
/// this venue without a human clicking approve on every trade" — a first-class, queryable property
/// rather than an assumption baked into the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradingModel {
    /// Caller's own wallet must sign every transaction (Panta's model) - human-approval-shaped.
    UserSignedPerTrade,
    /// A separate, revocable delegated key signs trades autonomously (Pascal's model) - suitable
    /// for unattended/automated/AI-driven trading.
    DelegatedTradingKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VenueCapabilities {
    pub framing: OutcomeFraming,
    pub trading_model: TradingModel,
    /// false for a bonding-curve venue like Panta (one quote endpoint gives you a direct fill
    /// estimate); true for an order-book venue like Pascal (a quote means walking the book).
    pub has_order_book: bool,
}

/// One market as a venue reports it, before any canonical normalization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueMarket {
    pub venue_market_id: String,
    pub title: String,
    pub category: String,
    /// Venue-native phase/status string (e.g. Panta's "primary"/"secondary", kept verbatim for
    /// debugging rather than forced into a shared enum that would lose venue-specific meaning).
    pub phase: String,
    pub start_time: i64,
    pub resolution_time: i64,
    pub yes_price: Option<String>,
    pub no_price: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ListParams {
    pub category: Option<String>,
    pub status: Option<String>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketPage {
    pub items: Vec<VenueMarket>,
    pub next_cursor: Option<String>,
}

/// A single-venue quote for a given side/amount — the thing the position engine compares across
/// venues. `side` is kept as the venue's own native string (e.g. "yes" or "long") since the
/// canonical layer, not the adapter, owns translating a normalized side into venue-native terms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueQuote {
    pub venue: &'static str,
    pub venue_market_id: String,
    pub side: String,
    pub amount_usdc: String,
    pub effective_shares_or_size: String,
    pub avg_price: String,
    pub fee_usdc: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("venue returned an error: {0}")]
    Venue(String),
    #[error("could not parse venue response: {0}")]
    Parse(String),
    #[error("market not found: {0}")]
    MarketNotFound(String),
}

#[async_trait]
pub trait VenueAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> VenueCapabilities;
    async fn list_markets(&self, params: ListParams) -> Result<MarketPage, AdapterError>;
    async fn get_market(&self, venue_market_id: &str) -> Result<VenueMarket, AdapterError>;
    async fn quote(
        &self,
        venue_market_id: &str,
        side: &str,
        amount_usdc: &str,
    ) -> Result<VenueQuote, AdapterError>;
}
