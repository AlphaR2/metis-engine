//! The one path every live quote takes: cache first, then the venue, with the outcome recorded
//! against venue health by what it means (see `HealthRegistry::record_error`). `/markets` and
//! `/find-best-market` both quote through here, so caching and health rules can't drift apart.

use std::sync::Arc;

use rust_decimal::Decimal;

use crate::errors::AdapterError;
use crate::models::venue::VenueQuote;
use crate::repositories::health_repository::HealthRegistry;
use crate::repositories::quote_cache::QuoteCache;
use crate::services::VenueAdapter;

pub async fn quote(
    adapter: &Arc<dyn VenueAdapter>,
    venue_market_id: &str,
    side: &str,
    size_usdc: Decimal,
    health: &HealthRegistry,
    cache: &QuoteCache,
) -> Result<VenueQuote, AdapterError> {
    let venue_id = adapter.id();
    if let Some(cached) = cache.get(venue_id, venue_market_id, side, size_usdc) {
        return Ok(cached);
    }
    match adapter.quote(venue_market_id, side, size_usdc).await {
        Ok(quote) => {
            health.record_success(venue_id);
            cache.insert(venue_id, venue_market_id, side, size_usdc, quote.clone());
            Ok(quote)
        }
        Err(err) => {
            health.record_error(venue_id, &err);
            Err(err)
        }
    }
}
