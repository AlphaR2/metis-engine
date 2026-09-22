//! Wire-format data shapes for Pascal's REST API - https://docs.pascal.trade. Field names taken
//! from Pascal's published API reference, confirmed this session. Prices/sizes are decimal
//! strings (6 d.p.) on the wire, not micro-unit integers like Jupiter.

use std::collections::HashMap;

use serde::Deserialize;

/// UI/event metadata nested inside a `PascalMarket`.
#[derive(Debug, Deserialize)]
pub struct PascalDisplayAttributes {
    pub event_description: String,
    pub market_description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub expected_event_start_time_ms: Option<String>,
    pub expected_resolution_time_ms: String,
}

/// One market from Pascal's `/markets` response.
#[derive(Debug, Deserialize)]
pub struct PascalMarket {
    pub symbol: String,
    pub display_attributes: PascalDisplayAttributes,
    pub mark_price: Option<String>,
}

/// Pascal's `/markets` response envelope.
#[derive(Debug, Deserialize)]
pub struct PascalMarketsResponse {
    #[serde(default)]
    pub markets: Vec<PascalMarket>,
}

/// One market's order book from Pascal's `/books` response - asks lowest-first, bids
/// highest-first, each level a `[price, size]` tuple of decimal strings.
#[derive(Debug, Deserialize)]
pub struct PascalBook {
    pub asks: Vec<(String, String)>,
    pub bids: Vec<(String, String)>,
}

/// Pascal's `/books` response envelope, keyed by symbol.
#[derive(Debug, Deserialize)]
pub struct PascalBooksResponse {
    pub books: HashMap<String, PascalBook>,
}
