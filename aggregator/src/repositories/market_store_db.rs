//! Postgres persistence for the market store.
//!
//! The in-memory `MarketStore` stays the read path - every `GET /markets` is served from it, so
//! a request never waits on the database. Postgres is the durability layer underneath: refresh
//! writes through, and boot reads back, so a restart serves the previous catalog immediately
//! instead of returning an empty list until the first refresh finishes.
//!
//! Queries are built with the runtime `sqlx::query` API rather than the `query!` macros on
//! purpose: the macros need a reachable `DATABASE_URL` (or a committed `.sqlx` directory) at
//! *compile* time, which would mean nobody can `cargo build` this crate without database access.

use sqlx::{PgPool, Row};

use crate::models::venue::{Category, VenueEvent, VenueMarket};

/// Replaces everything stored for one (venue, category) bucket, in a single transaction.
///
/// Delete-then-insert rather than upsert-and-reconcile: the refresh already computed the complete
/// current contents of this bucket, and a market the venue has stopped listing must disappear
/// rather than linger with a stale `last_synced_at`. Wrapping it in a transaction means a reader
/// never observes the empty window between the delete and the insert.
pub async fn replace_bucket(
    pool: &PgPool,
    venue_id: &str,
    category: &Category,
    markets: &[VenueMarket],
) -> Result<(), sqlx::Error> {
    let category_str = String::from(category.clone());
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM markets WHERE venue_id = $1 AND category = $2")
        .bind(venue_id)
        .bind(&category_str)
        .execute(&mut *tx)
        .await?;

    // Multi-row inserts, not one statement per market. A single Jupiter category can hold 4000+
    // markets, and a row-at-a-time loop makes that 4000+ sequential round-trips to a remote
    // database - slow enough to outlast the refresh interval itself.
    //
    // Chunked because Postgres caps a statement at 65535 bind parameters; at 12 columns per row
    // that allows ~5400, so 1000 leaves ample headroom.
    const ROWS_PER_INSERT: usize = 1_000;

    for chunk in markets.chunks(ROWS_PER_INSERT) {
        let mut builder = sqlx::QueryBuilder::new(
            "INSERT INTO markets (venue_id, venue_market_id, title, category, phase, start_time, \
             close_time, resolution_time, yes_price, no_price, liquidity, dead, \
             event_id, event_title, event_subtitle, event_image_url, rules) ",
        );
        builder.push_values(chunk, |mut row, market| {
            row.push_bind(market.venue_id.clone())
                .push_bind(market.venue_market_id.clone())
                .push_bind(market.title.clone())
                .push_bind(category_str.clone())
                .push_bind(market.phase.clone())
                .push_bind(market.start_time)
                .push_bind(market.close_time)
                .push_bind(market.resolution_time)
                .push_bind(market.yes_price.clone())
                .push_bind(market.no_price.clone())
                .push_bind(market.liquidity.map(|l| l.to_string()))
                .push_bind(market.dead)
                .push_bind(market.event.as_ref().map(|e| e.event_id.clone()))
                .push_bind(market.event.as_ref().map(|e| e.title.clone()))
                .push_bind(market.event.as_ref().and_then(|e| e.subtitle.clone()))
                .push_bind(market.event.as_ref().and_then(|e| e.image_url.clone()))
                .push_bind(market.rules.clone());
        });
        // A market can move between categories between refreshes, in which case the DELETE above
        // (scoped to the old category) won't have removed it - so the insert still needs to
        // upsert rather than conflict.
        builder.push(
            " ON CONFLICT (venue_id, venue_market_id) DO UPDATE SET \
             title = EXCLUDED.title, category = EXCLUDED.category, phase = EXCLUDED.phase, \
             start_time = EXCLUDED.start_time, close_time = EXCLUDED.close_time, \
             resolution_time = EXCLUDED.resolution_time, yes_price = EXCLUDED.yes_price, \
             no_price = EXCLUDED.no_price, liquidity = EXCLUDED.liquidity, dead = EXCLUDED.dead, \
             event_id = EXCLUDED.event_id, event_title = EXCLUDED.event_title, \
             event_subtitle = EXCLUDED.event_subtitle, event_image_url = EXCLUDED.event_image_url, \
             rules = EXCLUDED.rules, last_synced_at = now()",
        );
        builder.build().execute(&mut *tx).await?;
    }

    tx.commit().await
}

/// Loads every persisted market, grouped into (venue, category) buckets ready to seed the
/// in-memory store at boot.
///
/// No time filtering here: `MarketStore::markets_in` applies the betting window at read time
/// against the current clock, so persisting and loading stays a straight round-trip and the
/// freshness decision lives in exactly one place.
pub async fn load_all(pool: &PgPool) -> Result<Vec<(String, Category, Vec<VenueMarket>)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT venue_id, venue_market_id, title, category, phase, start_time, close_time, \
         resolution_time, yes_price, no_price, liquidity, dead, event_id, event_title, \
         event_subtitle, event_image_url, rules FROM markets",
    )
    .fetch_all(pool)
    .await?;

    let mut buckets: std::collections::HashMap<(String, String), Vec<VenueMarket>> =
        std::collections::HashMap::new();

    for row in rows {
        let venue_id: String = row.try_get("venue_id")?;
        let category: String = row.try_get("category")?;
        let liquidity: Option<String> = row.try_get("liquidity")?;
        // An event row only means anything if it names a question; a stored id with no title
        // would rebuild into an event that renders as a blank line.
        let event_id: Option<String> = row.try_get("event_id")?;
        let event_title: Option<String> = row.try_get("event_title")?;
        let event = match (event_id, event_title) {
            (Some(event_id), Some(title)) => Some(VenueEvent {
                event_id,
                title,
                subtitle: row.try_get("event_subtitle")?,
                image_url: row.try_get("event_image_url")?,
            }),
            _ => None,
        };
        let market = VenueMarket {
            venue_id: venue_id.clone(),
            venue_market_id: row.try_get("venue_market_id")?,
            title: row.try_get("title")?,
            event,
            rules: row.try_get("rules")?,
            category: Category::from(category.clone()),
            phase: row.try_get("phase")?,
            start_time: row.try_get("start_time")?,
            close_time: row.try_get("close_time")?,
            resolution_time: row.try_get("resolution_time")?,
            yes_price: row.try_get("yes_price")?,
            no_price: row.try_get("no_price")?,
            liquidity: liquidity.and_then(|l| l.parse().ok()),
            dead: row.try_get("dead")?,
        };
        buckets.entry((venue_id, category)).or_default().push(market);
    }

    Ok(buckets
        .into_iter()
        .map(|((venue, category), markets)| (venue, Category::from(category), markets))
        .collect())
}
