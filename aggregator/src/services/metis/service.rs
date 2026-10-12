//! The Metis venue: reads markets from the program on chain and builds unsigned transactions for
//! buying and claiming. Nothing here signs, and nothing holds a key.

use std::collections::HashMap;

use async_trait::async_trait;
use metis_core::{
    amm::state::{Market, MarketStatus},
    constants::{BOOK_SEED, METADATA_SEED},
};
use rust_decimal::Decimal;
use solana_pubkey::Pubkey;

use super::{
    chain::{Rpc, decode_market, decode_metadata},
    secondary::{Books, Depth, SeatHolding, buy_with, read_depth, seat_holding},
    view::{
        ID_NO, ID_YES, default_config, distributable, outcome_mint, payout_atoms, phase_of, primary_open, quote_side,
        secondary_open, side_index, side_name, status_of, to_atoms, to_venue_market, usdc,
    },
};
use crate::{
    errors::AdapterError,
    models::{
        execution::{BuyIntent, ClaimIntent, ConfirmOutcome, SellIntent, UnsignedTransaction, claim_ref},
        position::{Position, PositionState},
        venue::{
            ListParams, MarketPage, OutcomeFraming, TradingModel, VenueCapabilities, VenueMarket, VenueQuote,
        },
    },
    services::VenueAdapter,
    utils::time::now_unix,
};

/// How a Metis adapter is pointed at a deployment.
#[derive(Debug, Clone)]
pub struct MetisSettings {
    pub rpc_url: String,
    /// The deployed core program.
    pub program_id: Pubkey,
    /// The Config account. Defaults to the PDA derived from the trust roots this build was
    /// compiled with, which is only right if they match the deployed program's.
    pub config: Option<Pubkey>,
    /// The size of the bet the catalog's prices are quoted for, in USDC atoms.
    pub reference_deposit: u64,
}

impl MetisSettings {
    pub const DEFAULT_REFERENCE_DEPOSIT: u64 = 10_000_000;
}

pub struct MetisAdapter {
    pub(super) rpc: Rpc,
    pub(super) settings: MetisSettings,
}

impl MetisAdapter {
    pub fn new(settings: MetisSettings) -> Self {
        Self { rpc: Rpc::new(reqwest::Client::new(), settings.rpc_url.clone()), settings }
    }

    pub(super) fn config_address(&self) -> Pubkey {
        self.settings.config.unwrap_or_else(|| default_config(&self.settings.program_id))
    }

    /// The address of one outcome's order book: derived from the market, created by close_primary.
    pub(super) fn book_address(&self, market: &Pubkey, side: usize) -> Pubkey {
        Pubkey::find_program_address(&[BOOK_SEED, market.as_ref(), &[side as u8]], &self.settings.program_id).0
    }

    /// Both of a market's order books. A side whose book doesn't exist yet (close_primary hasn't
    /// run), or can't be read as the right pair, is `None`.
    pub(super) async fn books_of(&self, markets: &[(Pubkey, Market)]) -> Result<HashMap<Pubkey, Books>, AdapterError> {
        if markets.is_empty() {
            return Ok(HashMap::new());
        }
        let slot = self.rpc.slot().await?.min(u32::MAX as u64) as u32;
        let addresses: Vec<Pubkey> = markets.iter().flat_map(|(a, _)| [self.book_address(a, ID_YES), self.book_address(a, ID_NO)]).collect();
        let accounts = self.rpc.accounts(&addresses).await?;
        let mut out = HashMap::new();
        for (i, (address, market)) in markets.iter().enumerate() {
            let quote = Pubkey::new_from_array(market.quote_mint.to_bytes());
            let read = |side: usize| -> Option<Depth> {
                let data = accounts.get(i * 2 + side)?.as_ref()?;
                read_depth(data, &outcome_mint(&self.settings.program_id, address, side), &quote, slot)
            };
            out.insert(*address, [read(ID_YES), read(ID_NO)]);
        }
        Ok(out)
    }

    /// What `owner` has on each market's books beyond its token accounts: seat balances and resting
    /// orders. Only markets whose books exist (betting is over) are read; one batch for all.
    pub(super) async fn seats_of(&self, markets: &[(Pubkey, Market)], owner: &Pubkey) -> Result<HashMap<(Pubkey, usize), SeatHolding>, AdapterError> {
        let now = now_unix();
        let with_books: Vec<&(Pubkey, Market)> = markets.iter().filter(|(_, m)| now >= m.primary_end).collect();
        if with_books.is_empty() {
            return Ok(HashMap::new());
        }
        let addresses: Vec<Pubkey> = with_books.iter().flat_map(|(a, _)| [self.book_address(a, ID_YES), self.book_address(a, ID_NO)]).collect();
        let accounts = self.rpc.accounts(&addresses).await?;
        let mut out = HashMap::new();
        for (i, (address, market)) in with_books.iter().enumerate() {
            let quote = Pubkey::new_from_array(market.quote_mint.to_bytes());
            for side in [ID_YES, ID_NO] {
                let Some(Some(data)) = accounts.get(i * 2 + side) else { continue };
                if let Some(h) = seat_holding(data, owner, &outcome_mint(&self.settings.program_id, address, side), &quote).filter(|h| !h.is_empty()) {
                    out.insert((*address, side), h);
                }
            }
        }
        Ok(out)
    }

    /// A market and its text, by address. `MarketNotFound` if it isn't one of ours.
    pub(super) async fn load_market(
        &self,
        id: &str,
    ) -> Result<(Pubkey, metis_core::amm::state::Market, Option<metis_core::amm::metadata::MarketMetadata>), AdapterError> {
        let address: Pubkey = id.trim().parse().map_err(|_| AdapterError::MarketNotFound(id.to_string()))?;
        let data = self.rpc.account(&address).await?.ok_or_else(|| AdapterError::MarketNotFound(id.to_string()))?;
        let market = decode_market(&data).ok_or_else(|| AdapterError::MarketNotFound(id.to_string()))?;
        let (metadata_address, _) = Pubkey::find_program_address(&[METADATA_SEED, address.as_ref()], &self.settings.program_id);
        let metadata = self.rpc.account(&metadata_address).await?.and_then(|d| decode_metadata(&d));
        Ok((address, market, metadata))
    }
}

impl MetisAdapter {
    /// Quotes a buy against the order book. Same units as a primary quote - what the bet pays if it
    /// wins, and the break-even probability - and exact: once betting closes the pot is final.
    async fn quote_on_the_books(
        &self,
        venue_market_id: &str,
        address: &Pubkey,
        market: &Market,
        side: usize,
        atoms: u64,
        amount_usdc: Decimal,
    ) -> Result<VenueQuote, AdapterError> {
        let books = self.books_of(&[(*address, *market)]).await?;
        let depth = books
            .get(address)
            .and_then(|b| b[side].as_ref())
            .ok_or_else(|| AdapterError::InvalidRequest("this market's order book has not been opened yet".to_string()))?;
        if !depth.tradable() {
            return Err(AdapterError::InvalidRequest("this order book holds order types Metis cannot route yet".to_string()));
        }
        let fill = buy_with(depth, atoms);
        let pays = distributable(market) as u128;
        let payout = if market.outcome_shares[side] == 0 { 0 } else { (fill.base_atoms as u128 * pays / market.outcome_shares[side] as u128) as u64 };
        if fill.base_atoms == 0 || payout == 0 {
            return Err(AdapterError::NoLiquidity { venue_market_id: venue_market_id.to_string(), side: side_name(side).to_string() });
        }
        Ok(VenueQuote {
            venue: "metis",
            venue_market_id: venue_market_id.to_string(),
            side: side_name(side).to_string(),
            amount_usdc,
            effective_shares_or_size: usdc(payout),
            avg_price: (usdc(fill.quote_atoms) / usdc(payout)).round_dp(6),
            // The books charge nothing, and Metis takes nothing on a trade between holders.
            fee_usdc: Decimal::ZERO,
            fully_filled: fill.complete,
            expires_at: None,
        })
    }
}

#[async_trait]
impl VenueAdapter for MetisAdapter {
    fn id(&self) -> &'static str {
        "metis"
    }

    fn capabilities(&self) -> VenueCapabilities {
        VenueCapabilities {
            framing: OutcomeFraming::YesNo,
            trading_model: TradingModel::UserSignedPerTrade,
            // Once betting closes a market trades on Manifest's order books.
            has_order_book: true,
            supports_category_filter: false,
            // Every market comes back in one page, so there is no cursor to get stuck.
            cursor_pagination_advances: true,
            can_execute: true,
            // Two transactions (the market, then its text), on an oracle fixture that already exists.
            can_create_markets: true,
            can_list_positions: true,
            can_claim: true,
            can_sell: true,
        }
    }

    async fn list_markets(&self, params: ListParams) -> Result<MarketPage, AdapterError> {
        // Everything is returned in the first call; a later page has nothing more.
        if params.cursor.is_some() {
            return Ok(MarketPage { items: Vec::new(), next_cursor: None });
        }
        let program = self.settings.program_id;
        let (markets, mut metadata) = tokio::try_join!(self.rpc.markets(&program), self.rpc.metadata(&program))?;
        let now = now_unix();
        // Only markets past their primary window have books worth reading.
        let on_the_books: Vec<(Pubkey, Market)> = markets.iter().filter(|(_, m)| secondary_open(m, now)).cloned().collect();
        let books = self.books_of(&on_the_books).await?;
        let items = markets
            .into_iter()
            .filter_map(|(address, market)| {
                let text = metadata.remove(&address);
                to_venue_market(&address, &market, text.as_ref(), now, self.settings.reference_deposit, books.get(&address))
            })
            .collect();
        Ok(MarketPage { items, next_cursor: None })
    }

    async fn get_market(&self, venue_market_id: &str) -> Result<VenueMarket, AdapterError> {
        let (address, market, metadata) = self.load_market(venue_market_id).await?;
        let now = now_unix();
        let books = if secondary_open(&market, now) { self.books_of(&[(address, market)]).await? } else { HashMap::new() };
        to_venue_market(&address, &market, metadata.as_ref(), now, self.settings.reference_deposit, books.get(&address))
            .ok_or_else(|| AdapterError::MarketNotFound(format!("{venue_market_id} (no question has been written for it yet)")))
    }

    /// What a bet would really pay. `effective_shares_or_size` is how much the bet pays if it
    /// wins (so size times price is the amount staked, comparable with venues whose every share
    /// pays $1), and `avg_price` is the break-even probability. They are NOT the program's own
    /// share count and curve price: the program mints shares off a curve with virtual liquidity
    /// but pays winners from the real pot, so the curve's price is not the odds.
    async fn build_create_market(&self, intent: &crate::models::execution::CreateMarketIntent) -> Result<crate::models::execution::UnsignedTransaction, AdapterError> {
        self.build_create_market_transaction(intent).await
    }

    async fn quote(&self, venue_market_id: &str, side: &str, amount_usdc: Decimal) -> Result<VenueQuote, AdapterError> {
        let side_idx = side_index(side).ok_or_else(|| AdapterError::InvalidRequest(format!("unknown side '{side}' - Metis markets are yes/no")))?;
        let atoms = to_atoms(amount_usdc).filter(|a| *a > 0).ok_or_else(|| {
            AdapterError::InvalidRequest("amount_usdc must be a positive amount with at most 6 decimal places".to_string())
        })?;
        let (address, market, _) = self.load_market(venue_market_id).await?;
        let now = now_unix();
        if secondary_open(&market, now) {
            return self.quote_on_the_books(venue_market_id, &address, &market, side_idx, atoms, amount_usdc).await;
        }
        if !primary_open(&market, now) {
            return Err(AdapterError::InvalidRequest("this market is no longer trading".to_string()));
        }
        let quote = quote_side(&market, side_idx, atoms)
            .ok_or_else(|| AdapterError::InvalidRequest("that amount is too small to buy any shares".to_string()))?;
        if quote.payout_if_win == 0 {
            return Err(AdapterError::NoLiquidity { venue_market_id: venue_market_id.to_string(), side: side.to_string() });
        }
        let payout = usdc(quote.payout_if_win);
        Ok(VenueQuote {
            venue: "metis",
            venue_market_id: venue_market_id.to_string(),
            side: side_name(side_idx).to_string(),
            amount_usdc,
            effective_shares_or_size: payout,
            avg_price: (usdc(atoms) / payout).round_dp(6),
            fee_usdc: usdc(quote.fee),
            fully_filled: true,
            expires_at: None,
        })
    }

    async fn build_buy(&self, intent: &BuyIntent) -> Result<UnsignedTransaction, AdapterError> {
        self.build_buy_transaction(intent).await
    }

    async fn build_claim(&self, intent: &ClaimIntent) -> Result<UnsignedTransaction, AdapterError> {
        self.build_claim_transaction(intent).await
    }

    /// Sells a position on the market's order book, before the market resolves.
    async fn build_sell(&self, intent: &SellIntent) -> Result<UnsignedTransaction, AdapterError> {
        self.build_sell_transaction(intent).await
    }

    /// Every position the wallet holds: its balances of any Metis market's outcome tokens.
    async fn positions(&self, wallet: &str) -> Result<Vec<Position>, AdapterError> {
        let owner: Pubkey = wallet.trim().parse().map_err(|_| AdapterError::InvalidRequest("wallet is not a valid address".to_string()))?;
        let program = self.settings.program_id;
        let (markets, balances) = tokio::try_join!(self.rpc.markets(&program), self.rpc.token_balances(&owner))?;
        // Slices parked on a book (a maker's seat balance, or offered in a resting ask) are still the
        // wallet's, so they count as its position even when its token account is empty.
        let seats = self.seats_of(&markets, &owner).await?;
        if balances.is_empty() && seats.is_empty() {
            return Ok(Vec::new());
        }
        let mut titles: Option<HashMap<Pubkey, _>> = None;
        // Books are only read for markets the wallet actually holds shares in, and only if the
        // market is trading on them.
        let held: Vec<(Pubkey, Market)> = markets
            .iter()
            .filter(|(a, m)| {
                secondary_open(m, now_unix())
                    && [ID_YES, ID_NO].iter().any(|s| balances.contains_key(&outcome_mint(&program, a, *s)) || seats.contains_key(&(*a, *s)))
            })
            .cloned()
            .collect();
        let books = self.books_of(&held).await?;

        let mut positions = Vec::new();
        for (address, market) in &markets {
            for side in [ID_YES, ID_NO] {
                let mint = outcome_mint(&program, address, side);
                let seat = seats.get(&(*address, side)).cloned().unwrap_or_default();
                let wallet_shares = balances.get(&mint).copied().unwrap_or(0);
                let shares = wallet_shares.saturating_add(seat.slices());
                if shares == 0 {
                    continue;
                }
                // Only fetch the text if the wallet holds something here.
                let titles = match &titles {
                    Some(t) => t,
                    None => titles.insert(self.rpc.metadata(&program).await?),
                };
                let title = titles.get(address).and_then(|m| m.field(0)).and_then(|b| core::str::from_utf8(b).ok()).map(str::to_string);
                // Only slices in the wallet's own token account can be sold in one swap.
                let sellable = wallet_shares > 0 && books.get(address).and_then(|b| b[side].as_ref()).is_some_and(|d| d.tradable() && d.best_bid().is_some());
                let mut position = position_of(address, market, side, shares, title, wallet, sellable);
                position.open_orders = seat.open_orders;
                positions.push(position);
            }
            // A crowd-seeded market's backers hold the raise's token: that is their position (see dbc.rs).
            if let Some(backer_mint) = super::dbc::backer_mint(market) {
                let tokens = balances.get(&backer_mint).copied().unwrap_or(0);
                if tokens > 0 {
                    let titles = match &titles {
                        Some(t) => t,
                        None => titles.insert(self.rpc.metadata(&program).await?),
                    };
                    let title = titles.get(address).and_then(|m| m.field(0)).and_then(|b| core::str::from_utf8(b).ok()).map(str::to_string);
                    positions.push(super::dbc::backer_position(address, market, tokens, title, wallet));
                }
            }
        }
        Ok(positions)
    }

    /// Confirms a broadcast: reads the signature's state from the chain. There is no Metis server
    /// to tell, and nothing to register - the transaction either landed or it didn't.
    async fn confirm(&self, venue_ref: &str, signature: &str) -> Result<ConfirmOutcome, AdapterError> {
        use super::chain::SignatureState;
        use crate::models::execution::ActionStatus;
        let market = parse_ref(venue_ref).map(|(_, market, _)| market.to_string());
        let (status, message) = match self.rpc.signature_state(signature.trim()).await? {
            SignatureState::Landed => (ActionStatus::Confirmed, None),
            // Seen but not yet confirmed, or not seen yet: still on its way.
            SignatureState::Processed | SignatureState::Unknown => (ActionStatus::Submitted, None),
            SignatureState::Failed(why) => (ActionStatus::Failed, Some(format!("the transaction failed on chain: {why}"))),
        };
        Ok(ConfirmOutcome { venue_ref: venue_ref.to_string(), status, venue_market_id: market, message })
    }
}

/// A position's id: `<market>:<yes|no>`.
pub(super) fn position_ref(market: &Pubkey, side: usize) -> String {
    format!("{market}:{}", side_name(side))
}

pub(super) fn parse_position_ref(position: &str) -> Option<(Pubkey, usize)> {
    let (market, side) = position.split_once(':')?;
    Some((market.parse().ok()?, super::view::side_index(side)?))
}

/// `venue_ref` of a Metis action: `metis:<what>:<market>:<wallet>`.
pub(super) fn action_ref(what: &str, market: &Pubkey, wallet: &str) -> String {
    format!("metis:{what}:{market}:{wallet}")
}

pub(super) fn parse_ref(venue_ref: &str) -> Option<(&str, &str, &str)> {
    let mut parts = venue_ref.strip_prefix("metis:")?.splitn(3, ':');
    Some((parts.next()?, parts.next()?, parts.next()?))
}

pub(super) fn position_of(
    address: &Pubkey,
    market: &metis_core::amm::state::Market,
    side: usize,
    shares: u64,
    title: Option<String>,
    wallet: &str,
    sellable: bool,
) -> Position {
    let status = status_of(market);
    let payout = payout_atoms(market, side, shares);
    let winner = market.winning_outcome as usize;

    let (state, claim) = match status {
        Some(MarketStatus::Settled) if side == winner => (PositionState::Claimable, true),
        Some(MarketStatus::Settled) => (PositionState::Lost, false),
        // A cancelled market is refunded through the same claim call.
        Some(MarketStatus::Cancelled) => (PositionState::Cancelled, true),
        _ => (PositionState::Open, false),
    };
    Position {
        venue_id: "metis".to_string(),
        venue_market_id: address.to_string(),
        // What `POST /execution/sells` takes: the market and the side, since a wallet holds at most
        // one position per side of a market.
        venue_position_id: Some(position_ref(address, side)),
        side: side_name(side).to_string(),
        // The program's own shares - not payout-equivalent contracts. What they pay is payout_usdc.
        contracts: usdc(shares),
        // A wallet's cost is not stored on chain, so it stays unknown rather than guessed.
        avg_price_usdc: None,
        cost_usdc: None,
        value_usdc: None,
        value_is_estimate: false,
        pnl_usdc: None,
        payout_usdc: payout.map(usdc),
        state,
        phase: Some(phase_of(market, now_unix()).to_string()),
        open_orders: 0,
        claim_ref: claim.then(|| claim_ref(wallet, &address.to_string())),
        can_sell: sellable,
        title,
        event_id: Some(address.to_string()),
        event_title: None,
        updated_at: None,
    }
}
