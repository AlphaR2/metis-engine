//! Process bootstrap only: load env, init tracing, hand off to `app::run()`. All server
//! construction/wiring lives in `app.rs`.
//!
//! This service aggregates prediction markets across a curated venue network. `GET /markets`
//! serves raw venue markets by category, windowed on when betting closes and ranked by
//! price/liquidity/execution; `/events` keeps the canonical cross-venue event graph for when
//! several venues quote the same real-world event. See `README.md` for the venue-by-venue notes.

mod aggregator;
mod api;
mod app;
mod config;
mod engine;
mod database;
mod errors;
mod middleware;
mod metrics;
mod models;
mod openapi;
mod repositories;
mod services;
mod state;
#[cfg(test)]
mod test_support;
mod utils;
#[cfg(test)]
mod venue_contract;

use std::env;
use std::path::PathBuf;

/// Finds and loads the `.env` file, trying every location it could reasonably live in.
///
/// `dotenvy`'s own search only walks *up* from the current directory, which means running from
/// the workspace root - the natural thing to do with `cargo run -p metis-aggregator` - never
/// finds `aggregator/.env`, because that is a level *down*. The result is a server that boots
/// with no API keys and 401s against every venue, with nothing but a warning to explain it.
///
/// So: check the crate directory too (baked in at compile time), and let `ENV_FILE` override
/// everything for a deployment that keeps its config elsewhere.
fn load_environment() {
    let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(explicit) = env::var("ENV_FILE") {
        candidates.push(PathBuf::from(explicit));
    }
    candidates.push(current_dir.join(".env"));
    // This crate's own directory, so running from the workspace root works.
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env"));

    for candidate in &candidates {
        if dotenvy::from_path(candidate).is_ok() {
            println!("Loaded .env from: {}", candidate.display());
            return;
        }
    }

    // Last resort: dotenvy's own upward walk, for a layout none of the above anticipated.
    if let Ok(path) = dotenvy::dotenv() {
        println!("Loaded .env from: {}", path.display());
        return;
    }

    eprintln!("Warning: no .env file found. Looked in:");
    for candidate in &candidates {
        eprintln!("  {}", candidate.display());
    }
    eprintln!("Set ENV_FILE=/path/to/.env to point at one explicitly.");
    eprintln!("Continuing with system environment variables...");
}

#[tokio::main]
async fn main() {
    load_environment();

    // `from_default_env()` silently falls back to "error" only when RUST_LOG is unset, which
    // hides every info!/warn! log this service relies on to show boot progress - default to
    // "info" explicitly instead, matching the reference codebase's own telemetry init.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // `LOG_FORMAT=json` for prod log aggregation, mirroring mobile-server's toggle - plain text
    // otherwise, unchanged from what this service already logs in local dev.
    if env::var("LOG_FORMAT").as_deref() == Ok("json") {
        tracing_subscriber::fmt().json().with_env_filter(env_filter).init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }

    let config = config::Config::from_env();
    if let Err(e) = config.validate() {
        tracing::error!("configuration validation failed: {e}");
        std::process::exit(1);
    }

    app::run(config).await;
}
