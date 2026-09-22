//! One place that declares every env var this service reads - `Config::from_env()` builds it,
//! `.validate()` checks it's actually usable before the server starts.

use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub port: String,
    pub cors_origin: String,
    /// Empty when unset - every Panta call then fails, logged as a warning at boot.
    pub panta_api_key: String,
    pub panta_quote_wallet: String,
    pub panta_base_url: String,
    /// Empty when unset - every Jupiter call then fails with a 401, logged as a warning at boot.
    pub jupiter_api_key: String,
    pub jupiter_base_url: String,
    pub pascal_base_url: String,
    /// Which venues to actually wire up, by id ("panta", "jupiter", "pascal") - lets a venue be
    /// tested/rolled out independently of the others without a code change. Comma-separated,
    /// e.g. `ENABLED_VENUES=panta` or `ENABLED_VENUES=panta,jupiter`.
    pub enabled_venues: Vec<String>,
    /// Postgres connection string. Optional for now, not required: unset means the server runs
    /// in-memory-only (today's behavior) rather than refusing to boot - lets persistence land and
    /// be wired in ahead of the hosted DB actually being provisioned. Once repositories are fully
    /// DB-backed this should become a hard requirement instead.
    pub database_url: Option<String>,
    /// `sqlx::PgPoolOptions` max connections - default matches mobile-server's `DATABASE_POOL_SIZE`.
    pub database_pool_size: u32,
    /// How often the background refresh re-pulls every venue, in seconds. Without this the
    /// catalog is only ever as fresh as the boot-time pull plus whatever manual `POST /refresh`
    /// calls happen to land.
    ///
    /// Default 600. A full measured pass takes 3-4 minutes (Panta ~20s, then Jupiter's ten
    /// categories at ~25s each, paced to stay under its rate limit), so a shorter interval would
    /// leave the service refreshing almost continuously. What this refresh captures - which
    /// markets exist and when they close - changes slowly; live pricing comes from `quote_size`
    /// on the request path, not from here.
    pub refresh_interval_secs: u64,
    /// Whether the aggregator will build buy transactions at all.
    ///
    /// **Default false.** Off means the execution routes refuse and no venue order call is made.
    /// A buy spends real USDC and cannot be undone, so it is switched on deliberately rather than
    /// arriving with a deploy.
    pub execution_enabled: bool,
    /// Whether the aggregator will build market-creation transactions.
    ///
    /// **Default false**, for the same reason and one more: a created market is a permanent public
    /// artifact carrying the creating wallet's name, and there is no delete.
    pub market_creation_enabled: bool,
    /// Shared secret for the operator-only routes (`POST /refresh`, `GET /metrics`). Unset means
    /// those routes are refused outright rather than left open - a refresh is a multi-minute
    /// fan-out across every venue, which is not something an anonymous caller should be able to
    /// trigger at will.
    pub admin_token: Option<String>,
}

/// The venues wired up when `ENABLED_VENUES` is unset.
///
/// Pascal is deliberately absent: its wire format is unverified (its `/markets` envelope is
/// `{status, data}`, which the current model does not match) and its adapter hardcodes
/// `dead: false`, so including it by default would mean serving markets we cannot filter for
/// tradeability. Add it explicitly once its schema has had the same confirmation pass as the
/// other two.
fn default_enabled_venues() -> Vec<String> {
    vec!["panta".to_string(), "jupiter".to_string()]
}

impl Config {
    /// Reads every setting from the environment, falling back to sane MVP defaults.
    pub fn from_env() -> Self {
        Self {
            port: env::var("PORT").unwrap_or_else(|_| "8080".to_string()),
            cors_origin: env::var("CORS_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".to_string()),
            panta_api_key: env::var("PANTA_API_KEY").unwrap_or_default(),
            panta_quote_wallet: env::var("PANTA_QUOTE_WALLET").unwrap_or_default(),
            panta_base_url: env::var("PANTA_BASE_URL")
                .unwrap_or_else(|_| "https://live-api.panta.market/api/v1".to_string()),
            jupiter_api_key: env::var("JUPITER_API_KEY").unwrap_or_default(),
            jupiter_base_url: env::var("JUPITER_BASE_URL")
                .unwrap_or_else(|_| "https://api.jup.ag/prediction/v1".to_string()),
            pascal_base_url: env::var("PASCAL_BASE_URL")
                .unwrap_or_else(|_| "https://data.pascal.trade/api/v1".to_string()),
            enabled_venues: env::var("ENABLED_VENUES")
                .ok()
                .map(|raw| raw.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_else(default_enabled_venues),
            database_url: env::var("DATABASE_URL").ok(),
            database_pool_size: env::var("DATABASE_POOL_SIZE")
                .ok()
                .and_then(|raw| raw.parse().ok())
                .unwrap_or(15),
            refresh_interval_secs: env::var("REFRESH_INTERVAL_SECS")
                .ok()
                .and_then(|raw| raw.parse().ok())
                .filter(|secs| *secs >= 30)
                .unwrap_or(600),
            // Anything other than an explicit "true" is off - a typo must fail closed, never open.
            execution_enabled: env::var("EXECUTION_ENABLED")
                .map(|v| v.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            market_creation_enabled: env::var("MARKET_CREATION_ENABLED")
                .map(|v| v.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            admin_token: env::var("ADMIN_TOKEN").ok().filter(|t| !t.trim().is_empty()),
        }
    }

    /// Checks the config is actually usable - a malformed `CORS_ORIGIN` would otherwise only
    /// surface as a panic deep inside axum's CORS layer construction.
    pub fn validate(&self) -> Result<(), String> {
        self.cors_origin
            .parse::<axum::http::HeaderValue>()
            .map_err(|e| format!("CORS_ORIGIN '{}' is not a valid header value: {e}", self.cors_origin))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(cors_origin: &str) -> Config {
        Config {
            port: "8080".to_string(),
            cors_origin: cors_origin.to_string(),
            panta_api_key: String::new(),
            panta_quote_wallet: String::new(),
            panta_base_url: "https://live-api.panta.market/api/v1".to_string(),
            jupiter_api_key: String::new(),
            jupiter_base_url: "https://api.jup.ag/prediction/v1".to_string(),
            pascal_base_url: "https://data.pascal.trade/api/v1".to_string(),
            enabled_venues: default_enabled_venues(),
            database_url: None,
            database_pool_size: 15,
            refresh_interval_secs: 600,
            execution_enabled: false,
            market_creation_enabled: false,
            admin_token: None,
        }
    }

    #[test]
    fn a_normal_origin_validates() {
        assert!(config_with("http://localhost:3000").validate().is_ok());
    }

    #[test]
    fn an_origin_with_invalid_header_bytes_fails_validation() {
        assert!(config_with("http://\ninvalid").validate().is_err());
    }

    #[test]
    fn money_moving_flags_are_off_unless_explicitly_true() {
        // Read directly rather than through from_env(), which would depend on process-wide state.
        let parse = |raw: Option<&str>| {
            raw.map(|v| v.trim().eq_ignore_ascii_case("true")).unwrap_or(false)
        };
        assert!(!parse(None), "unset must be off");
        assert!(!parse(Some("")), "empty must be off");
        assert!(!parse(Some("1")), "1 is not true - only the word counts");
        assert!(!parse(Some("yes")), "yes is not true");
        assert!(parse(Some("true")), "the word itself turns it on");
        assert!(parse(Some("  TRUE  ")), "case and surrounding space are tolerated");
    }
}
