//! Scaffold only. This is the integration surface for third-party
//! providers — apps that plug into Openstake rather than building their own
//! settlement engine. Openstake is the venue; a provider is a front-end
//! that lets its own users interact with Openstake's markets.
//!
//! Off-chain only. Never compiled to SBF, never a dependency of
//! `programs/core`. Trait stubs below map 1:1 onto the four things a
//! provider needs to do:
//!
//! a. fetch and display Openstake's existing markets
//! b. let their users create markets — these are Openstake markets
//!    underneath; the provider collects its own fee on top
//! c. place bets on behalf of their users, primary or secondary market
//! d. cash out / settle across whatever markets their users participated in
//!
//! No implementation yet — this exists so later crates (and the eventual
//! CLOB/secondary-market work) can be designed against a stable shape.

pub struct MarketSummary {
    pub market_id: [u8; 32],
    pub num_outcomes: u8,
    pub status: u8,
}

pub trait OpenstakeProvider {
    /// (a) List markets a provider's front-end can display to its users.
    fn list_markets(&self) -> Vec<MarketSummary>;

    /// (b) Create a new Openstake market on a provider's behalf, tagging it
    /// with whatever fee arrangement lets the provider collect its cut.
    fn create_market_for_provider(&self, provider_fee_bps: u16) -> [u8; 32];

    /// (c) Place a bet for one of the provider's users, against either the
    /// primary (AMM) or secondary (CLOB, not yet built) market.
    fn place_bet(&self, market_id: [u8; 32], outcome_index: u8, amount: u64) -> Result<(), String>;

    /// (d) Cash out / claim on behalf of a user across whichever markets
    /// they participated in.
    fn cashout(&self, market_id: [u8; 32], user: [u8; 32]) -> Result<u64, String>;
}
