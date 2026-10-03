//! Test doubles shared across modules. Compiled only for tests.

use std::sync::Mutex;

use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::venue::{
    Category, ListParams, MarketPage, OutcomeFraming, TradingModel, VenueCapabilities, VenueMarket, VenueQuote,
};
use crate::services::VenueAdapter;
use crate::utils::time::now_unix;

/// A venue that serves `pages` pages of `per_page` markets each, and fails every request for
/// any page listed in `failing`. `generation` is baked into market ids so a test can tell
/// which refresh a stored market came from.
pub struct ScriptedVenue {
    categorized: bool,
    pages: usize,
    per_page: usize,
    failing: Mutex<Vec<usize>>,
    generation: Mutex<u32>,
    /// Live quotes actually sent to this venue (cache hits don't count).
    #[allow(dead_code)]
    pub quotes_taken: Mutex<u32>,
    /// Category names `filter_categories` returns (when `categorized`).
    pub filters: Mutex<Vec<String>>,
    /// Filters answered with a 400, as a venue does for a category it won't filter on.
    pub rejected_filters: Mutex<Vec<String>>,
    /// A category only the unfiltered sweep reveals: when set, the sweep also returns one market
    /// reporting it, as Panta's markets report categories its `/categories/` omits.
    pub sweep_only_category: Mutex<Option<String>>,
    /// Page requests received, successful or not.
    pub list_calls: Mutex<u32>,
}

impl ScriptedVenue {
    pub fn new(categorized: bool, pages: usize) -> Self {
        Self {
            categorized,
            pages,
            per_page: 3,
            failing: Mutex::new(vec![]),
            generation: Mutex::new(1),
            quotes_taken: Mutex::new(0),
            filters: Mutex::new(vec!["sports".to_string()]),
            rejected_filters: Mutex::new(vec![]),
            sweep_only_category: Mutex::new(None),
            list_calls: Mutex::new(0),
        }
    }

    pub fn fail_pages(&self, pages: &[usize]) {
        *self.failing.lock().unwrap() = pages.to_vec();
    }

    /// This venue as the registry would hand it to `Aggregator::new`, with default settings.
    pub fn registered(self: &std::sync::Arc<Self>) -> crate::services::registry::RegisteredVenue {
        crate::services::registry::RegisteredVenue {
            adapter: self.clone(),
            settings: crate::services::registry::VenueSettings::default(),
        }
    }

    pub fn next_generation(&self) {
        *self.generation.lock().unwrap() += 1;
    }
}

#[async_trait]
impl VenueAdapter for ScriptedVenue {
    fn id(&self) -> &'static str {
        "scripted"
    }

    fn capabilities(&self) -> VenueCapabilities {
        VenueCapabilities {
            framing: OutcomeFraming::YesNo,
            trading_model: TradingModel::UserSignedPerTrade,
            has_order_book: false,
            supports_category_filter: self.categorized,
            cursor_pagination_advances: true,
            can_execute: false,
            can_create_markets: false,
            can_list_positions: false,
            can_claim: false,
            can_sell: false,
        }
    }

    async fn filter_categories(&self) -> Result<Vec<String>, AdapterError> {
        Ok(if self.categorized { self.filters.lock().unwrap().clone() } else { vec![] })
    }

    async fn list_markets(&self, params: ListParams) -> Result<MarketPage, AdapterError> {
        *self.list_calls.lock().unwrap() += 1;
        let page: usize = params.cursor.as_deref().map_or(0, |c| c.parse().unwrap());
        if self.failing.lock().unwrap().contains(&page) {
            return Err(AdapterError::Venue("500 upstream exploded".to_string()));
        }
        if let Some(category) = &params.category
            && self.rejected_filters.lock().unwrap().contains(category)
        {
            return Err(AdapterError::Venue(format!(
                "HTTP status client error (400 Bad Request) for url (https://scripted.test/markets?category={category})"
            )));
        }
        let generation = *self.generation.lock().unwrap();
        let now = now_unix();
        let items = (0..self.per_page)
            .map(|i| VenueMarket {
                venue_id: "scripted".to_string(),
                venue_market_id: format!("g{generation}-p{page}-m{i}"),
                title: format!("Market {page}/{i}"),
                event: None,
                rules: None,
                category: Category::from("sports"),
                phase: "open".to_string(),
                start_time: now - 60,
                close_time: now + 86_400,
                resolution_time: now + 2 * 86_400,
                yes_price: Some("0.5".to_string()),
                no_price: Some("0.5".to_string()),
                liquidity: None,
                dead: false,
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let mut items = items;
        if params.category.is_none()
            && page == 0
            && let Some(hidden) = self.sweep_only_category.lock().unwrap().clone()
        {
            items.push(VenueMarket {
                venue_market_id: format!("g{generation}-{hidden}"),
                title: format!("Only in {hidden}"),
                category: Category::from(hidden.as_str()),
                venue_category: hidden,
                close_time: now + 86_400,
                resolution_time: now + 2 * 86_400,
                ..Default::default()
            });
        }
        let next_cursor = (page + 1 < self.pages).then(|| (page + 1).to_string());
        Ok(MarketPage { items, next_cursor })
    }

    async fn get_market(&self, id: &str) -> Result<VenueMarket, AdapterError> {
        Err(AdapterError::MarketNotFound(id.to_string()))
    }

    async fn quote(&self, id: &str, side: &str, amount: Decimal) -> Result<VenueQuote, AdapterError> {
        *self.quotes_taken.lock().unwrap() += 1;
        Ok(VenueQuote {
            venue: "scripted",
            venue_market_id: id.to_string(),
            side: side.to_string(),
            amount_usdc: amount,
            effective_shares_or_size: amount * Decimal::from(2),
            avg_price: Decimal::new(5, 1),
            fee_usdc: Decimal::ZERO,
            fully_filled: true,
            expires_at: None,
        })
    }

}


/// Serves recorded venue responses over real HTTP, so an adapter runs unmodified against them.
///
/// Reads `fixtures/<venue>/_index.tsv` (`name`, `status`, `path?query` per line, as written by
/// `scripts/capture-fixtures.sh`) and answers each request whose path and query match a recorded
/// one - query parameters in any order - with that recording's body and status. Anything not
/// recorded is a 404, which is itself how a venue answers an unknown path.
pub struct FixtureServer {
    pub base: String,
    _server: tokio::task::JoinHandle<()>,
}

impl FixtureServer {
    pub async fn start(venue_dir: &str) -> Self {
        use std::collections::HashMap;

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(venue_dir);
        let index = std::fs::read_to_string(dir.join("_index.tsv")).expect("fixture index present");
        let mut responses: HashMap<String, (u16, String)> = HashMap::new();
        for line in index.lines() {
            let mut cols = line.splitn(3, '\t');
            let (Some(name), Some(status), Some(target)) = (cols.next(), cols.next(), cols.next()) else { continue };
            let Ok(body) = std::fs::read_to_string(dir.join(format!("{name}.json"))) else { continue };
            responses.insert(normalize_target(target), (status.parse().unwrap_or(200), body));
        }
        let responses = std::sync::Arc::new(responses);

        let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let responses = responses.clone();
            async move {
                let target = uri.path_and_query().map_or(uri.path().to_string(), |pq| pq.as_str().to_string());
                match responses.get(&normalize_target(&target)) {
                    Some((status, body)) => (
                        axum::http::StatusCode::from_u16(*status).unwrap_or(axum::http::StatusCode::OK),
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        body.clone(),
                    ),
                    None => (
                        axum::http::StatusCode::NOT_FOUND,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        format!("{{\"error\": \"no fixture for {target}\"}}"),
                    ),
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind fixture server");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("fixture server");
        });
        Self { base, _server: server }
    }
}

/// `path?query` with the leading slash dropped, percent-decoding undone and parameters sorted, so
/// a recording matches however the adapter orders or encodes its query.
fn normalize_target(target: &str) -> String {
    let target = target.trim_start_matches('/');
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut pairs: Vec<(String, String)> = reqwest::Url::parse(&format!("http://x/?{query}"))
        .map(|u| u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect())
        .unwrap_or_default();
    pairs.sort();
    let query: Vec<String> = pairs.into_iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{path}?{}", query.join("&"))
}

/// When the fixtures were captured - tests over them freeze the clock here.
pub fn fixtures_captured_at() -> i64 {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join("captured_at");
    std::fs::read_to_string(path).expect("fixtures/captured_at present").trim().parse().expect("unix seconds")
}
