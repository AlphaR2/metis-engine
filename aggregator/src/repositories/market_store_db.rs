//! Postgres persistence for the market store.
//!
//! The in-memory `MarketStore` stays the read path - every `GET /markets` is served from it, so
//! a request never waits on the database. Postgres is the durability layer underneath: refresh
//! writes through, and boot reads back, so a restart serves the previous catalog immediately
//! instead of returning an empty list until the first refresh finishes.
//!
//! Each venue is stored as one zstd-compressed JSON snapshot (`market_snapshots`), not a row per
//! market. Nothing reads individual markets from the database, and the catalog is mostly text
//! repeated across markets (a Jupiter event's title and rules on every one of its markets), which
//! only compresses well as a whole: ~67 MB of rows becomes a few MB, and on a remote database the
//! load is bound by bytes on the wire.
//!
//! Queries are built with the runtime `sqlx::query` API rather than the `query!` macros on
//! purpose: the macros need a reachable `DATABASE_URL` (or a committed `.sqlx` directory) at
//! *compile* time, which would mean nobody can `cargo build` this crate without database access.

use sqlx::{PgPool, Row};

use crate::models::venue::{Category, VenueMarket};

/// The format of `market_snapshots.payload`. A row with any other value is skipped at load.
const ENCODING: &str = "json+zstd";

/// zstd level. The snapshot is written every refresh, so this trades a little size for speed:
/// the long tail of higher levels saves a few hundred KB and costs seconds of CPU.
const ZSTD_LEVEL: i32 = 9;

/// Replaces the stored snapshot for one venue.
///
/// The whole catalog, not a merge: the refresh already computed the venue's complete current
/// catalog, and a market (or a whole category) the venue has stopped listing must disappear.
pub async fn replace_venue(
    pool: &PgPool,
    venue_id: &str,
    by_category: &[(Category, Vec<VenueMarket>)],
) -> Result<(), sqlx::Error> {
    let market_count: usize = by_category.iter().map(|(_, markets)| markets.len()).sum();
    let json = serde_json::to_vec(by_category).map_err(|err| sqlx::Error::Encode(err.into()))?;
    // Compressing tens of MB is CPU work; keep it off the async workers.
    let payload = tokio::task::spawn_blocking(move || compress(&json))
        .await
        .map_err(|err| sqlx::Error::Encode(err.into()))?
        .map_err(|err| sqlx::Error::Encode(err.into()))?;

    sqlx::query(
        "INSERT INTO market_snapshots (venue_id, encoding, market_count, payload, written_at) \
         VALUES ($1, $2, $3, $4, now()) \
         ON CONFLICT (venue_id) DO UPDATE SET encoding = EXCLUDED.encoding, \
         market_count = EXCLUDED.market_count, payload = EXCLUDED.payload, written_at = now()",
    )
    .bind(venue_id)
    .bind(ENCODING)
    .bind(i32::try_from(market_count).unwrap_or(i32::MAX))
    .bind(payload)
    .execute(pool)
    .await?;
    Ok(())
}

/// Loads every stored snapshot, grouped into (venue, category) buckets ready to seed the
/// in-memory store at boot.
///
/// No time filtering here: `MarketStore::markets_in` applies the betting window at read time
/// against the current clock, so persisting and loading stays a straight round-trip and the
/// freshness decision lives in exactly one place.
///
/// A snapshot in an unknown encoding, or one the current model can't read, is logged and
/// skipped: that venue starts empty and its next refresh rewrites it.
pub async fn load_all(pool: &PgPool) -> Result<Vec<(String, Category, Vec<VenueMarket>)>, sqlx::Error> {
    let rows = sqlx::query("SELECT venue_id, encoding, payload FROM market_snapshots").fetch_all(pool).await?;

    let mut buckets = Vec::new();
    for row in rows {
        let venue_id: String = row.try_get("venue_id")?;
        let encoding: String = row.try_get("encoding")?;
        if encoding != ENCODING {
            tracing::warn!("{venue_id}: skipping stored snapshot in unknown encoding {encoding:?}");
            continue;
        }
        let payload: Vec<u8> = row.try_get("payload")?;
        // Decompressing and parsing tens of MB is CPU work; keep it off the async workers.
        let decoded = tokio::task::spawn_blocking(move || decode(&payload))
            .await
            .map_err(|err| sqlx::Error::Decode(err.into()))?;
        match decoded {
            Ok(by_category) => {
                buckets.extend(by_category.into_iter().map(|(category, markets)| (venue_id.clone(), category, markets)))
            }
            Err(err) => tracing::warn!("{venue_id}: skipping stored snapshot that doesn't decode: {err}"),
        }
    }
    Ok(buckets)
}

fn compress(json: &[u8]) -> std::io::Result<Vec<u8>> {
    zstd::encode_all(json, ZSTD_LEVEL)
}

fn decode(payload: &[u8]) -> std::io::Result<Vec<(Category, Vec<VenueMarket>)>> {
    let json = zstd::decode_all(payload)?;
    Ok(serde_json::from_slice(&json)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips() {
        let market = VenueMarket {
            venue_id: "jupiter".into(),
            venue_market_id: "m1".into(),
            title: "Will it rain?".into(),
            category: Category::from("science".to_string()),
            venue_category: "climate & science".into(),
            rules: Some("Resolves YES if it rains.".into()),
            ..Default::default()
        };
        let by_category = vec![(market.category.clone(), vec![market.clone(), market])];

        let decoded = decode(&compress(&serde_json::to_vec(&by_category).unwrap()).unwrap()).unwrap();

        assert_eq!(serde_json::to_value(&decoded).unwrap(), serde_json::to_value(&by_category).unwrap());
    }

    #[test]
    fn garbage_payload_is_an_error_not_a_panic() {
        assert!(decode(b"not zstd").is_err());
    }
}
