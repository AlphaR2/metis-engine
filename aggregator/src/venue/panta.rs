//! Panta: a non-custodial, binary YES/NO bonding-curve market on Solana.
//! https://docs.panta.market — base URL, endpoint shapes, and field names below are taken
//! directly from their published API reference, confirmed this session.

use async_trait::async_trait;
use serde::Deserialize;

use super::{
    AdapterError, ListParams, MarketPage, OutcomeFraming, TradingModel, VenueAdapter,
    VenueCapabilities, VenueMarket, VenueQuote,
};

const BASE_URL: &str = "https://live-api.panta.market/api/v1";

pub struct PantaAdapter {
    client: reqwest::Client,
    api_key: String,
    /// Panta's quote endpoint requires a buyer wallet even for a pure quote (no trade is
    /// executed at this stage) - a fixed quoting wallet is fine here since nothing is signed
    /// or moved; a real per-user wallet only matters once actual execution is built.
    quote_wallet: String,
}

impl PantaAdapter {
    pub fn new(api_key: String, quote_wallet: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            quote_wallet,
        }
    }
}

// ── Wire types, matching Panta's documented JSON exactly ───────────────────────────────────

#[derive(Debug, Deserialize)]
struct PantaMarketRow {
    #[serde(rename = "marketId")]
    market_id: String,
    title: String,
    category: String,
    phase: String,
    #[serde(rename = "startTime")]
    start_time: i64,
    #[serde(rename = "resolutionTime")]
    resolution_time: i64,
    #[serde(rename = "yesPrice")]
    yes_price: Option<String>,
    #[serde(rename = "noPrice")]
    no_price: Option<String>,
}

impl From<PantaMarketRow> for VenueMarket {
    fn from(row: PantaMarketRow) -> Self {
        VenueMarket {
            venue_market_id: row.market_id,
            title: row.title,
            category: row.category,
            phase: row.phase,
            start_time: row.start_time,
            resolution_time: row.resolution_time,
            yes_price: row.yes_price,
            no_price: row.no_price,
        }
    }
}

#[derive(Debug, Deserialize)]
struct PantaListResponse {
    items: Vec<PantaMarketRow>,
    #[serde(rename = "nextCursor")]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PantaQuoteResponse {
    shares: String,
    #[serde(rename = "avgPrice")]
    avg_price: String,
    #[serde(rename = "feeUsdc")]
    fee_usdc: String,
    #[serde(rename = "expiresAt")]
    expires_at: String,
}

#[async_trait]
impl VenueAdapter for PantaAdapter {
    fn id(&self) -> &'static str {
        "panta"
    }

    fn capabilities(&self) -> VenueCapabilities {
        VenueCapabilities {
            framing: OutcomeFraming::YesNo,
            trading_model: TradingModel::UserSignedPerTrade,
            has_order_book: false,
        }
    }

    async fn list_markets(&self, params: ListParams) -> Result<MarketPage, AdapterError> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(category) = params.category {
            query.push(("category", category));
        }
        if let Some(status) = params.status {
            query.push(("status", status));
        }
        if let Some(cursor) = params.cursor {
            query.push(("cursor", cursor));
        }

        let response = self
            .client
            .get(format!("{BASE_URL}/markets/"))
            .header("X-Api-Key", &self.api_key)
            .query(&query)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| AdapterError::Venue(e.to_string()))?
            .json::<PantaListResponse>()
            .await
            .map_err(|e| AdapterError::Parse(e.to_string()))?;

        Ok(MarketPage {
            items: response.items.into_iter().map(Into::into).collect(),
            next_cursor: response.next_cursor,
        })
    }

    async fn get_market(&self, venue_market_id: &str) -> Result<VenueMarket, AdapterError> {
        let response = self
            .client
            .get(format!("{BASE_URL}/markets/{venue_market_id}/"))
            .header("X-Api-Key", &self.api_key)
            .send()
            .await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AdapterError::MarketNotFound(venue_market_id.to_string()));
        }

        let row = response
            .error_for_status()
            .map_err(|e| AdapterError::Venue(e.to_string()))?
            .json::<PantaMarketRow>()
            .await
            .map_err(|e| AdapterError::Parse(e.to_string()))?;

        Ok(row.into())
    }

    async fn quote(
        &self,
        venue_market_id: &str,
        side: &str,
        amount_usdc: &str,
    ) -> Result<VenueQuote, AdapterError> {
        let body = serde_json::json!({
            "wallet": self.quote_wallet,
            "marketId": venue_market_id,
            "side": side,
            "amountUsdc": amount_usdc,
        });

        let response = self
            .client
            .post(format!("{BASE_URL}/primaryorderquote/"))
            .header("X-Api-Key", &self.api_key)
            .json(&body)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| AdapterError::Venue(e.to_string()))?
            .json::<PantaQuoteResponse>()
            .await
            .map_err(|e| AdapterError::Parse(e.to_string()))?;

        Ok(VenueQuote {
            venue: "panta",
            venue_market_id: venue_market_id.to_string(),
            side: side.to_string(),
            amount_usdc: amount_usdc.to_string(),
            effective_shares_or_size: response.shares,
            avg_price: response.avg_price,
            fee_usdc: response.fee_usdc,
            expires_at: Some(response.expires_at),
        })
    }
}
