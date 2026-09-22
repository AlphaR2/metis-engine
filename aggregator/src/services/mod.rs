//! Business logic - internal domain services (`matching`, `routing`), the pure numeric engines
//! backing them (`maths`), and per-vendor venue integrations (`panta`, `jupiter`, `pascal`), each
//! split into `service.rs` (business logic, including any side/venue-specific trading decisions)
//! + `client.rs` (raw HTTP/wire calls only).

pub mod jupiter;
pub mod matching;
pub mod markets;
pub mod maths;
pub mod panta;
pub mod pascal;
pub mod routing;

use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::execution::{
    BuyIntent, ClaimIntent, ConfirmOutcome, CreateMarketIntent, UnsignedTransaction,
};
use crate::models::venue::{Category, ListParams, MarketPage, VenueCapabilities, VenueMarket, VenueQuote};

/// The one interface every venue integration implements - Panta, Jupiter, Pascal.
#[async_trait]
pub trait VenueAdapter: Send + Sync {
    /// This venue's short, stable identifier, e.g. "panta".
    fn id(&self) -> &'static str;
    /// What this venue can do - framing, trading model, order book or not.
    fn capabilities(&self) -> VenueCapabilities;
    /// Lists this venue's markets, one page at a time.
    async fn list_markets(&self, params: ListParams) -> Result<MarketPage, AdapterError>;
    /// Fetches one market by this venue's own id for it. Part of the adapter contract; no route
    /// exposes a single-market lookup yet.
    #[allow(dead_code)]
    async fn get_market(&self, venue_market_id: &str) -> Result<VenueMarket, AdapterError>;
    /// Prices buying `amount_usdc` of `side` on this market right now.
    async fn quote(
        &self,
        venue_market_id: &str,
        side: &str,
        amount_usdc: Decimal,
    ) -> Result<VenueQuote, AdapterError>;
    /// Translates a canonical `Category` into this venue's own native filter string, or `None`
    /// if this venue has no equivalent for it.
    ///
    /// The two integrated venues accept genuinely different vocabularies (Jupiter 400s on
    /// anything outside its own list; Panta has buckets Jupiter lacks and vice versa), so this
    /// cannot be one shared list. Returning `None` means "skip this category for this venue",
    /// which is why a caller asking for `esports` gets Jupiter markets and no Panta error.
    ///
    /// The default returns `None` for everything, meaning "no server-side category filter" - the
    /// refresh then fetches once and buckets by each market's own reported category.
    fn native_category(&self, _category: &Category) -> Option<String> {
        None
    }

    /// Fetches a raw GET from this venue's API and returns the body **exactly as sent** - no
    /// parsing, no filtering, no normalisation.
    ///
    /// This exists to answer "what does the venue actually return", which no amount of reading our
    /// own models can tell you: every other path through this service has already dropped dead
    /// rows, filled in fallbacks and reshaped fields by the time you see anything. When a venue's
    /// data looks wrong, this is what distinguishes *their* bug from *ours*.
    ///
    /// GET only, deliberately: it borrows the venue credentials, and a general relay that could
    /// also POST would be a way to place orders through someone else's key.
    async fn raw_get(&self, _path: &str, _query: &[(String, String)]) -> Result<String, AdapterError> {
        Err(AdapterError::Unsupported { venue: self.id(), action: "raw inspection" })
    }

    // ── Actions that need a signature ───────────────────────────────────────────────────────
    //
    // Each of these builds an unsigned transaction and hands it back. None of them signs, and none
    // of them broadcasts - the caller's wallet does both. That is what keeps Metis non-custodial
    // even once it can trade.
    //
    // They default to `Unsupported` so a venue implements only what it genuinely offers, and a
    // caller asking for something a venue cannot do gets told exactly that rather than a failure
    // that looks like an outage. `capabilities()` advertises which are real.

    /// Builds a buy for `intent`, priced against a fresh quote taken at build time.
    async fn build_buy(&self, _intent: &BuyIntent) -> Result<UnsignedTransaction, AdapterError> {
        Err(AdapterError::Unsupported { venue: self.id(), action: "buying" })
    }

    /// Builds a new-market creation for `intent`. The signing wallet pays the fee and earns any
    /// creator fees the market later accrues.
    async fn build_create_market(
        &self,
        _intent: &CreateMarketIntent,
    ) -> Result<UnsignedTransaction, AdapterError> {
        Err(AdapterError::Unsupported { venue: self.id(), action: "market creation" })
    }

    /// Builds a claim for winnings or accrued creator fees.
    async fn build_claim(&self, _intent: &ClaimIntent) -> Result<UnsignedTransaction, AdapterError> {
        Err(AdapterError::Unsupported { venue: self.id(), action: "claims" })
    }

    /// Tells the venue a built transaction was broadcast, and asks what became of it.
    ///
    /// Keyed on `venue_ref` and the signature, both of which the venue already knows, so calling
    /// it twice with the same pair is safe - Panta's register is explicitly idempotent on exactly
    /// that. A dropped response must never mean a caller is afraid to retry.
    async fn confirm(
        &self,
        _venue_ref: &str,
        _signature: &str,
    ) -> Result<ConfirmOutcome, AdapterError> {
        Err(AdapterError::Unsupported { venue: self.id(), action: "confirmation" })
    }
}
