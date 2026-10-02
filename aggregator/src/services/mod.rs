//! Business logic - internal domain services (`matching`, `routing`), the pure numeric engines
//! backing them (`maths`), and per-vendor venue integrations (`panta`, `jupiter`), each
//! split into `service.rs` (business logic, including any side/venue-specific trading decisions)
//! + `client.rs` (raw HTTP/wire calls only).

pub mod jupiter;
pub mod maths;
pub mod panta;
pub mod positions;
pub mod quoting;
pub mod registry;
pub mod routing;

use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::execution::{
    BuyIntent, ClaimIntent, ConfirmOutcome, CreateMarketIntent, SellIntent, UnsignedTransaction,
};
use crate::models::position::Position;
use crate::models::venue::{ListParams, MarketPage, VenueCapabilities, VenueMarket, VenueQuote};

/// The one interface every venue integration implements - Panta, Jupiter.
#[async_trait]
pub trait VenueAdapter: Send + Sync {
    /// This venue's short, stable identifier, e.g. "panta".
    fn id(&self) -> &'static str;
    /// What this venue can do - framing, trading model, order book or not.
    fn capabilities(&self) -> VenueCapabilities;
    /// Lists this venue's markets, one page at a time.
    async fn list_markets(&self, params: ListParams) -> Result<MarketPage, AdapterError>;
    /// Fetches one market by this venue's own id for it, live from the venue (served by
    /// `GET /v1/markets/{venue}/{id}/live`). May lack fields only the list carries - Jupiter's
    /// bare market has no category - so callers overlay what the catalog holds.
    async fn get_market(&self, venue_market_id: &str) -> Result<VenueMarket, AdapterError>;
    /// Prices buying `amount_usdc` of `side` on this market right now.
    async fn quote(
        &self,
        venue_market_id: &str,
        side: &str,
        amount_usdc: Decimal,
    ) -> Result<VenueQuote, AdapterError>;
    /// The venue's own category names that its list filter accepts, verbatim - read from the
    /// venue where it publishes them (Panta's `/categories/`). The refresh runs one chain per name
    /// plus one unfiltered sweep, and passes each name back unchanged in `ListParams::category`.
    ///
    /// Markets are always bucketed by the category they report themselves, so this only decides
    /// how the catalog is fetched, never how it is labelled. The default (empty) means no
    /// server-side category filter: the unfiltered sweep is the whole catalog.
    async fn filter_categories(&self) -> Result<Vec<String>, AdapterError> {
        Ok(Vec::new())
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
    /// also POST would be a way to place orders through someone else's key. Returns the upstream
    /// status code alongside the body so a venue error is not dressed up as a 200.
    async fn raw_get(&self, _path: &str, _query: &[(String, String)]) -> Result<(u16, String), AdapterError> {
        Err(AdapterError::Unsupported { venue: self.id(), action: "raw inspection" })
    }

    /// First path segments `raw_get` may reach on this venue - read-only catalog endpoints only.
    /// Anything else on the venue's host (order routes, account data, other products sharing the
    /// same API key) stays out of reach even for an operator. Empty means no raw inspection.
    fn raw_path_prefixes(&self) -> &'static [&'static str] {
        &[]
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

    /// Every position `wallet` holds on this venue, normalised. Read-only.
    ///
    /// An error means the venue could not be read, which is **not** the same as the wallet holding
    /// nothing: an implementation must never turn a failure into an empty list, because a portfolio
    /// that silently loses a venue reads as lost money.
    async fn positions(&self, _wallet: &str) -> Result<Vec<Position>, AdapterError> {
        Err(AdapterError::Unsupported { venue: self.id(), action: "listing positions" })
    }

    /// Builds a sell that exits a position before its market resolves.
    async fn build_sell(&self, _intent: &SellIntent) -> Result<UnsignedTransaction, AdapterError> {
        Err(AdapterError::Unsupported { venue: self.id(), action: "selling" })
    }

    /// Builds a claim for winnings or accrued creator fees.
    async fn build_claim(&self, _intent: &ClaimIntent) -> Result<UnsignedTransaction, AdapterError> {
        Err(AdapterError::Unsupported { venue: self.id(), action: "claims" })
    }

    /// Reports where a previously built action stands. Read-only: it must never tell the venue
    /// anything, so polling it as often as a caller likes is always safe. A venue with no
    /// read-only status lookup says `Unsupported` rather than guessing (audit A8).
    async fn status(&self, _venue_ref: &str) -> Result<ConfirmOutcome, AdapterError> {
        Err(AdapterError::Unsupported { venue: self.id(), action: "status lookup" })
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
