//! Postgres persistence for the model's event-grouping decisions (see the `market_subjects` migration).

use std::collections::HashMap;

use sqlx::{PgPool, Row};

use crate::engine::ai::Cached;

pub async fn load(pool: &PgPool) -> Result<HashMap<String, Cached>, sqlx::Error> {
    let rows = sqlx::query("SELECT market_key, content_hash, join_key, group_key, title FROM market_subjects").fetch_all(pool).await?;
    let mut out = HashMap::new();
    for row in rows {
        out.insert(
            row.try_get::<String, _>("market_key")?,
            Cached {
                content_hash: row.try_get("content_hash")?,
                join: row.try_get("join_key")?,
                group: row.try_get("group_key")?,
                title: row.try_get("title")?,
            },
        );
    }
    Ok(out)
}

pub async fn save(pool: &PgPool, entries: &[(String, Cached)], model: &str) -> Result<(), sqlx::Error> {
    for (key, c) in entries {
        sqlx::query(
            "INSERT INTO market_subjects (market_key, content_hash, join_key, group_key, title, model) VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (market_key) DO UPDATE SET content_hash = EXCLUDED.content_hash, join_key = EXCLUDED.join_key, \
             group_key = EXCLUDED.group_key, title = EXCLUDED.title, model = EXCLUDED.model, created_at = now()",
        )
        .bind(key)
        .bind(&c.content_hash)
        .bind(&c.join)
        .bind(&c.group)
        .bind(&c.title)
        .bind(model)
        .execute(pool)
        .await?;
    }
    Ok(())
}
