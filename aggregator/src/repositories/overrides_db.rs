//! Postgres persistence for operator match overrides (see the `match_overrides` migration).

use sqlx::{PgPool, Row};

use crate::engine::matching::MatchOverrides;

/// The pair in its stored order.
fn ordered(a: &str, b: &str) -> (String, String) {
    if a <= b { (a.to_string(), b.to_string()) } else { (b.to_string(), a.to_string()) }
}

pub async fn load(pool: &PgPool) -> Result<MatchOverrides, sqlx::Error> {
    let rows = sqlx::query("SELECT left_listing, right_listing, decision FROM match_overrides ORDER BY created_at")
        .fetch_all(pool)
        .await?;
    let mut overrides = MatchOverrides::default();
    for row in rows {
        let pair: (String, String) = (row.try_get("left_listing")?, row.try_get("right_listing")?);
        match row.try_get::<String, _>("decision")?.as_str() {
            "merge" => overrides.merge.push(pair),
            _ => overrides.split.push(pair),
        }
    }
    Ok(overrides)
}

/// Records `merge` or `split` for a pair, replacing any earlier decision about it.
pub async fn upsert(pool: &PgPool, left: &str, right: &str, merge: bool) -> Result<(), sqlx::Error> {
    let (left, right) = ordered(left, right);
    sqlx::query(
        "INSERT INTO match_overrides (left_listing, right_listing, decision) VALUES ($1, $2, $3) \
         ON CONFLICT (left_listing, right_listing) DO UPDATE SET decision = EXCLUDED.decision, created_at = now()",
    )
    .bind(left)
    .bind(right)
    .bind(if merge { "merge" } else { "split" })
    .execute(pool)
    .await?;
    Ok(())
}
