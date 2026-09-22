//! Postgres connection pool construction. Mirrors mobile-server's `database/connection.rs` role
//! (one place that builds the pool), adapted to sqlx's async-native pool instead of Diesel's
//! r2d2 + `web::block` pattern - this service is axum/tokio from the ground up, so there's no
//! blocking-executor boundary to bridge.
//!
//! Migrations are run as an explicit deploy step (`sqlx migrate run`), not embedded/auto-run at
//! binary startup - the same convention mobile-server actually follows (its embedded-migration
//! code exists but is commented out and unused), not run automatically here either.

use sqlx::postgres::{PgPool, PgPoolOptions};

/// Builds the Postgres connection pool. Returns `Err` on a malformed `DATABASE_URL` or an
/// unreachable database - callers decide whether that's fatal (a hard-required DB) or a
/// degraded-mode warning (persistence optional for now, see `Config::database_url`).
pub async fn connect(database_url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    // The path to the hosted DB is flaky from some networks (resets/slow handshakes seen from
    // WSL) - a few short attempts beat one long wait, and beat giving up on the first reset.
    const ATTEMPTS: u32 = 3;
    let mut attempt = 1;
    loop {
        let result = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(std::time::Duration::from_secs(8))
            .connect(database_url)
            .await;
        match result {
            Ok(pool) => return Ok(pool),
            Err(err) if attempt < ATTEMPTS => {
                tracing::warn!("postgres connect failed (attempt {attempt}/{ATTEMPTS}): {err} - retrying");
                tokio::time::sleep(std::time::Duration::from_secs(u64::from(attempt))).await;
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}
