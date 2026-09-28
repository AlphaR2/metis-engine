//! Panta: a non-custodial, binary YES/NO bonding-curve market on Solana. Business-logic half of
//! the integration - the `VenueAdapter` impl. Raw HTTP/wire calls live in `client.rs`.

use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::execution::{ConfirmOutcome, CreateMarketIntent, UnsignedTransaction};
use crate::models::venue::panta::PantaMarketRow;
use crate::models::venue::{
    ListParams, MarketPage, OutcomeFraming, TradingModel, VenueCapabilities, VenueMarket, VenueQuote,
};
use crate::services::VenueAdapter;
use crate::utils::http::client_with_timeout;

use super::{chain, client, create};

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
    /// Detail rows already fetched, by market id, with when. See `reconcile_with_details`.
    details: std::sync::Mutex<std::collections::HashMap<String, (i64, PantaMarketRow)>>,
    /// Where markets are discovered. `None` falls back to the API's list, which misses markets.
    chain: Option<ChainSource>,
}

/// A Solana RPC and the Panta program to read markets from.
#[derive(Debug, Clone)]
pub struct ChainSource {
    pub rpc_url: String,
    pub program_id: String,
}

impl PantaAdapter {
    /// Builds an adapter for the given base URL, API key, and quoting wallet. With `chain`, markets
    /// are discovered on-chain (`discover`); without it, from the API's list.
    pub fn new(base_url: String, api_key: String, quote_wallet: String, chain: Option<ChainSource>) -> Self {
        Self {
            client: client_with_timeout(),
            base_url,
            api_key,
            quote_wallet,
            details: std::sync::Mutex::new(std::collections::HashMap::new()),
            chain,
        }
    }

    /// The single-market record, cached (see `reconcile_with_details`). `None` when the call
    /// fails - the caller keeps what it has.
    async fn detail(&self, market_id: &str, now: i64) -> Option<PantaMarketRow> {
        const DETAIL_TTL_SECS: i64 = 120;
        let cached = self
            .details
            .lock()
            .expect("detail cache lock poisoned")
            .get(market_id)
            .filter(|(at, detail)| detail.is_terminal() || now - at < DETAIL_TTL_SECS)
            .map(|(_, detail)| detail.clone());
        if cached.is_some() {
            return cached;
        }
        match client::get_market_row(&self.client, &self.base_url, &self.api_key, market_id).await {
            Ok(detail) => {
                self.details.lock().expect("detail cache lock poisoned").insert(market_id.to_string(), (now, detail.clone()));
                Some(detail)
            }
            Err(err) => {
                tracing::warn!("panta: could not fetch details for {market_id}: {err}");
                None
            }
        }
    }

    /// Every open market, found on-chain and described by the API.
    ///
    /// The chain decides *which* markets exist and their state; the API's record by id supplies
    /// category, images and its own text. For markets outside Panta's registry the API returns a
    /// placeholder - blank text and a stale `primary`/`open` - so the chain's question, rules,
    /// state and last price are used instead. The chain's state always wins: the placeholder's is
    /// not true.
    async fn discover(&self, source: &ChainSource) -> Result<Vec<PantaMarketRow>, AdapterError> {
        let now = crate::utils::time::now_unix();
        let events = chain::list_events(&self.client, &source.rpc_url, &source.program_id).await?;
        let open: Vec<chain::ChainEvent> = events.into_iter().filter(|e| e.is_open(now)).collect();
        let details = futures::future::join_all(open.iter().map(|e| self.detail(&e.market_id, now))).await;
        Ok(open.iter().zip(details).map(|(event, detail)| merge_chain(event, detail, now)).collect())
    }

    /// Panta's list rows are not trustworthy on their own (`docs/venue-fields.md`): they carry no
    /// text, and their state lags - rows listed `primary`/`open` turn out `cancelled` or
    /// `resolved` on the single-market endpoint. So every row the list says is still open is
    /// reconciled with its detail record, which supplies the question, rules and true state.
    ///
    /// Details are cached: a terminal state never changes, so it is kept for good; anything else
    /// is reused for `DETAIL_TTL_SECS`, which dedupes the same market seen by several chains in
    /// one refresh without letting its state go stale across refreshes. A failed detail call
    /// leaves the row as listed rather than failing the page.
    async fn reconcile_with_details(&self, rows: &mut [PantaMarketRow]) {
        let now = crate::utils::time::now_unix();
        for row in rows.iter_mut() {
            if !VenueMarket::from(row.clone()).is_open(now) {
                continue;
            }
            if let Some(detail) = self.detail(&row.market_id, now).await {
                row.reconcile_with(&detail);
            }
        }
    }
}

/// One open on-chain market as a catalog row: the API's record where it has one, with the chain's
/// state, and the chain's text and prices wherever the API has none.
fn merge_chain(event: &chain::ChainEvent, detail: Option<PantaMarketRow>, now: i64) -> PantaMarketRow {
    let mut row = detail.unwrap_or_default();
    row.market_id = event.market_id.clone();
    if row.text().is_none() {
        row.question = Some(event.question.clone());
    }
    if row.resolution_rule.as_deref().is_none_or(|r| r.trim().is_empty()) && !event.rules.is_empty() {
        row.resolution_rule = Some(event.rules.clone());
    }
    if row.sources.is_empty() {
        row.sources = event.sources.clone();
    }
    if row.category.trim().is_empty() {
        row.category = "other".to_string();
    }
    let phase = event.phase(now).map(chain::ChainPhase::as_str).map(str::to_string);
    row.phase = phase.clone().unwrap_or_else(|| if row.phase.is_empty() { "secondary".into() } else { row.phase.clone() });
    row.status = phase.map_or_else(|| row.status.clone(), |_| "open".to_string());
    row.resolved = false;
    row.start_time = event.start_time;
    row.end_time = Some(event.end_time);
    if row.resolution_time < event.end_time {
        row.resolution_time = event.end_time;
    }
    let priced = [&row.yes_price, &row.no_price, &row.primary_yes_price, &row.primary_no_price].iter().any(|p| p.is_some());
    if !priced && let Some(yes) = event.last_yes_price {
        row.yes_price = Some(yes.to_string());
        row.no_price = Some((Decimal::ONE - yes).normalize().to_string());
    }
    if row.volume_usdc.is_none() {
        row.volume_usdc = Some(event.total_volume_usdc.to_string());
    }
    row
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
            // Verified live 2026-09-28: nextCursor now advances and pages return disjoint rows
            // (previously it repeated page 1 forever - see the historical note on this field).
            cursor_pagination_advances: true,
            // The primary-buy flow (orders quote/build/submit/verify) exists.
            can_execute: true,
            // markets/create quote -> build -> register, with creator fees accruing
            // to whoever signs.
            can_create_markets: true,
        }
    }

    fn raw_path_prefixes(&self) -> &'static [&'static str] {
        &["markets"]
    }

    async fn raw_get(&self, path: &str, query: &[(String, String)]) -> Result<(u16, String), AdapterError> {
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

    async fn filter_categories(&self) -> Result<Vec<String>, AdapterError> {
        // On-chain discovery finds every market in the one unfiltered pass.
        if self.chain.is_some() {
            return Ok(Vec::new());
        }
        client::list_categories(&self.client, &self.base_url, &self.api_key).await
    }

    async fn list_markets(&self, params: ListParams) -> Result<MarketPage, AdapterError> {
        if let Some(source) = &self.chain {
            if params.cursor.is_some() || params.category.is_some() {
                return Ok(MarketPage { items: Vec::new(), next_cursor: None });
            }
            let rows = self.discover(source).await?;
            return Ok(MarketPage { items: rows.into_iter().map(Into::into).collect(), next_cursor: None });
        }
        let (mut rows, next_cursor) = client::list_markets(&self.client, &self.base_url, &self.api_key, params).await?;
        self.reconcile_with_details(&mut rows).await;
        Ok(MarketPage { items: rows.into_iter().map(Into::into).collect(), next_cursor })
    }

    async fn get_market(&self, venue_market_id: &str) -> Result<VenueMarket, AdapterError> {
        Ok(client::get_market_row(&self.client, &self.base_url, &self.api_key, venue_market_id).await?.into())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::panta::chain::ChainEvent;

    const NOW: i64 = 1_790_251_200;

    fn event(primary_phase_end: i64) -> ChainEvent {
        ChainEvent {
            market_id: "Evt1".into(),
            question: "Will Dangote Refinery be valued at over $60 billion before March 31, 2027?".into(),
            rules: "Resolve YES, if the valuation exceeds $60B. Else resolve NO.".into(),
            sources: vec!["af-news-bbc-africa".into()],
            start_time: NOW - 86_400 * 90,
            end_time: NOW + 86_400 * 180,
            resolved: false,
            cancelled: false,
            primary_phase_end: Some(primary_phase_end),
            last_yes_price: Some(Decimal::new(436, 3)),
            total_volume_usdc: Decimal::new(76_529, 0),
        }
    }

    /// What `GET /markets/{id}/` returns for a market outside Panta's registry.
    fn placeholder() -> PantaMarketRow {
        PantaMarketRow { market_id: "Evt1".into(), category: "sports".into(), phase: "primary".into(), status: "open".into(), ..Default::default() }
    }

    #[test]
    fn a_placeholder_detail_takes_the_chains_text_state_and_price() {
        let market: VenueMarket = merge_chain(&event(NOW - 1), Some(placeholder()), NOW).into();
        assert!(market.title.starts_with("Will Dangote Refinery"), "{}", market.title);
        assert!(market.rules.as_deref().unwrap().starts_with("Resolve YES"));
        assert_eq!(market.phase, "secondary", "the placeholder's primary/open is not true");
        assert!(market.display_only && !market.dead);
        assert!(market.is_bettable(NOW));
        assert_eq!(market.yes_price.as_deref(), Some("0.436"));
        assert_eq!(market.no_price.as_deref(), Some("0.564"));
        assert_eq!(market.liquidity, Some(Decimal::new(76_529, 0)));
    }

    #[test]
    fn a_full_detail_keeps_the_apis_text_and_prices() {
        let detail = PantaMarketRow {
            title: "Will Dangote Refinery be valued at over $60 billion?".into(),
            images: vec!["https://img/1.png".into()],
            yes_price: Some("0.44".into()),
            no_price: Some("0.56".into()),
            volume_usdc: Some("76529.00".into()),
            ..placeholder()
        };
        let market: VenueMarket = merge_chain(&event(NOW - 1), Some(detail), NOW).into();
        assert_eq!(market.title, "Will Dangote Refinery be valued at over $60 billion?");
        assert_eq!(market.yes_price.as_deref(), Some("0.44"));
        assert_eq!(market.event.unwrap().image_url.as_deref(), Some("https://img/1.png"));
    }

    #[test]
    fn a_market_in_its_primary_phase_is_tradable() {
        let market: VenueMarket = merge_chain(&event(NOW + 3600), None, NOW).into();
        assert_eq!(market.phase, "primary");
        assert!(!market.display_only && !market.dead);
        assert_eq!(market.venue_category, "other", "no API record: an honest default category");
    }

    /// Runs discovery against mainnet and the live API. Needs `PANTA_API_KEY` (read from
    /// `aggregator/.env`): `cargo test -p metis-aggregator live_discovery -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "hits mainnet RPC and Panta's live API"]
    async fn live_discovery() {
        let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env"));
        let source = ChainSource {
            rpc_url: std::env::var("PANTA_RPC_URL").unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into()),
            program_id: chain::PROGRAM_ID.into(),
        };
        let adapter = PantaAdapter::new(
            "https://live-api.panta.market/api/v1".into(),
            std::env::var("PANTA_API_KEY").expect("PANTA_API_KEY"),
            String::new(),
            Some(source),
        );
        let page = adapter.list_markets(ListParams::default()).await.expect("discovery");
        for m in &page.items {
            println!("{:9} display_only={:5} yes={:?} vol={:?} [{}] {}", m.phase, m.display_only, m.yes_price, m.liquidity, m.venue_category, m.title);
        }
        assert!(!page.items.is_empty());
        assert!(page.items.iter().all(|m| !m.title.is_empty() && !m.dead));
    }
}
