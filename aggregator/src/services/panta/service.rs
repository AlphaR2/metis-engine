//! Panta: a non-custodial, binary YES/NO bonding-curve market on Solana. Business-logic half of
//! the integration - the `VenueAdapter` impl. Raw HTTP/wire calls live in `client.rs`.

use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::execution::{ConfirmOutcome, CreateMarketIntent, UnsignedTransaction};
use crate::models::venue::{
    Category, ListParams, MarketPage, OutcomeFraming, TradingModel, VenueCapabilities, VenueMarket, VenueQuote,
};
use crate::services::VenueAdapter;
use crate::utils::http::client_with_timeout;

use super::{client, create};

/// Talks to Panta's REST API on behalf of `VenueAdapter`. Base URL and credentials all come from
/// `Config`, passed in at construction - nothing is hardcoded here.
pub struct PantaAdapter {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    /// Panta's quote endpoint requires a buyer wallet even for a pure quote (no trade is
    /// executed at this stage) - a fixed quoting wallet is fine here since nothing is signed
    /// or moved; a real per-user wallet only matters once actual execution is built.
    quote_wallet: String,
}

impl PantaAdapter {
    /// Builds an adapter for the given base URL, API key, and quoting wallet.
    pub fn new(base_url: String, api_key: String, quote_wallet: String) -> Self {
        Self {
            client: client_with_timeout(),
            base_url,
            api_key,
            quote_wallet,
        }
    }
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
            supports_category_filter: true,
            cursor_pagination_advances: false,
            // The primary-buy flow (orders quote/build/submit/verify) exists.
            can_execute: true,
            // markets/create quote -> build -> register, with creator fees accruing
            // to whoever signs.
            can_create_markets: true,
        }
    }

    async fn raw_get(&self, path: &str, query: &[(String, String)]) -> Result<String, AdapterError> {
        client::raw_get(&self.client, &self.base_url, &self.api_key, path, query).await
    }

    async fn build_create_market(
        &self,
        intent: &CreateMarketIntent,
    ) -> Result<UnsignedTransaction, AdapterError> {
        create::build_create_market(&self.client, &self.base_url, &self.api_key, intent).await
    }

    async fn confirm(
        &self,
        venue_ref: &str,
        signature: &str,
    ) -> Result<ConfirmOutcome, AdapterError> {
        // The prefix says which flow this reference belongs to, so no stored state is needed to
        // route a confirmation to the right endpoint.
        if let Some(create_id) = venue_ref.strip_prefix(create::CREATE_REF_PREFIX) {
            return create::register(&self.client, &self.base_url, &self.api_key, create_id, signature)
                .await;
        }
        Err(AdapterError::InvalidRequest(format!(
            "unrecognised panta reference '{venue_ref}' — expected a '{}' prefix",
            create::CREATE_REF_PREFIX
        )))
    }

    fn native_category(&self, category: &Category) -> Option<String> {
        crate::models::venue::panta::category_to_native(category).map(str::to_string)
    }

    async fn list_markets(&self, params: ListParams) -> Result<MarketPage, AdapterError> {
        client::list_markets(&self.client, &self.base_url, &self.api_key, params).await
    }

    async fn get_market(&self, venue_market_id: &str) -> Result<VenueMarket, AdapterError> {
        client::get_market(&self.client, &self.base_url, &self.api_key, venue_market_id).await
    }

    async fn quote(
        &self,
        venue_market_id: &str,
        side: &str,
        amount_usdc: Decimal,
    ) -> Result<VenueQuote, AdapterError> {
        client::quote(
            &self.client,
            &self.base_url,
            &self.api_key,
            &self.quote_wallet,
            venue_market_id,
            side,
            amount_usdc,
        )
        .await
    }

}
