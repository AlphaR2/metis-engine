//! One place that declares every env var this service reads - `Config::from_env()` builds it,
//! `.validate()` checks it's actually usable before the server starts.
//!
//! The MVP keeps this deliberately short: venue keys, a mode, a port and two optional extras
//! (a database and a Solana RPC for Panta discovery). Everything else has a fixed default.

use std::collections::HashMap;
use std::env;

/// One enabled venue's settings, read generically from `<ID>_*` environment variables so a new
/// venue needs no new config code: `<ID>_KIND` (which adapter, default: the id itself),
/// `<ID>_BASE_URL`, `<ID>_API_KEY`, and any adapter- or refresh-specific option such as
/// `<ID>_QUOTE_WALLET` or `<ID>_PAGE_CAP`.
#[derive(Clone, Debug)]
pub struct VenueConfig {
    pub id: String,
    pub kind: String,
    pub base_url: Option<String>,
    /// Empty when unset. Jupiter's catalog reads work without one (the header is then omitted);
    /// Panta's do not, and fail with a warning at boot.
    pub api_key: String,
    /// Every other `<ID>_<NAME>` variable, keyed by `<NAME>`.
    pub options: HashMap<String, String>,
}

impl VenueConfig {
    /// Reads `<ID>_*` from `vars` (the process environment in production).
    pub fn from_vars(id: &str, vars: &HashMap<String, String>) -> Self {
        let prefix = format!("{}_", id.to_uppercase().replace('-', "_"));
        let options: HashMap<String, String> = vars
            .iter()
            .filter_map(|(k, v)| k.strip_prefix(&prefix).map(|name| (name.to_string(), v.trim().to_string())))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        Self {
            id: id.to_string(),
            kind: options.get("KIND").cloned().unwrap_or_else(|| id.to_string()),
            base_url: options.get("BASE_URL").cloned(),
            api_key: options.get("API_KEY").cloned().unwrap_or_default(),
            options,
        }
    }

    pub fn option(&self, name: &str) -> Option<String> {
        self.options.get(name).cloned()
    }

    pub fn number(&self, name: &str) -> Option<u64> {
        self.options.get(name).and_then(|v| v.parse().ok())
    }

    pub fn env_prefix(&self) -> String {
        self.id.to_uppercase().replace('-', "_")
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub port: String,
    /// Origins allowed to call the API, comma-separated `scheme://host[:port]`. Unset allows any
    /// origin: the API holds no cookies or credentials, so there is nothing for another site to ride.
    pub cors_origin: Option<String>,
    /// The venues to wire up, in `ENABLED_VENUES` order (comma-separated ids, default
    /// `panta,jupiter`). Each reads its own `<ID>_*` settings; see `VenueConfig`.
    pub venues: Vec<VenueConfig>,
    /// Extra category aliases, `from=to,from=to`, on top of `CategoryAliases::defaults()`.
    pub category_aliases: String,
    /// Postgres connection string. Optional: unset means the server runs in-memory-only.
    pub database_url: Option<String>,
    /// `sqlx::PgPoolOptions` max connections.
    pub database_pool_size: u32,
    /// How often the background refresh re-pulls every venue, in seconds. Default 600: a full pass
    /// takes 3-4 minutes, and what it captures (which markets exist, when they close) changes
    /// slowly; live pricing comes from the live routes, not from here.
    pub refresh_interval_secs: u64,
    /// Whether this deployment builds transactions (buys, sells, claims, market creation).
    /// `MODE=read` turns all of it off and leaves a read-only catalog; anything else, including
    /// unset, is write. The aggregator never signs or broadcasts either way.
    pub write_mode: bool,
    /// Longest any single API request may run before it is answered 408. Default 45s: long
    /// enough for a live-quote fan-out, short enough that a stuck venue can't pin connections open.
    pub request_timeout_secs: u64,
}

/// The venues wired up when `ENABLED_VENUES` is unset.
fn default_enabled_venues() -> Vec<String> {
    vec!["panta".to_string(), "jupiter".to_string()]
}

impl Config {
    /// Reads every setting from the environment, falling back to MVP defaults.
    pub fn from_env() -> Self {
        let vars: HashMap<String, String> = env::vars().collect();
        let enabled_venues: Vec<String> = env::var("ENABLED_VENUES")
            .ok()
            .map(|raw| raw.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_else(default_enabled_venues);
        Self {
            port: env::var("PORT").unwrap_or_else(|_| "8080".to_string()),
            cors_origin: env::var("CORS_ORIGIN").ok().filter(|v| !v.trim().is_empty()),
            venues: enabled_venues.iter().map(|id| VenueConfig::from_vars(id, &vars)).collect(),
            category_aliases: env::var("CATEGORY_ALIASES").unwrap_or_default(),
            database_url: env::var("DATABASE_URL").ok().filter(|v| !v.trim().is_empty()),
            database_pool_size: env::var("DATABASE_POOL_SIZE").ok().and_then(|raw| raw.parse().ok()).unwrap_or(15),
            refresh_interval_secs: env::var("REFRESH_INTERVAL_SECS")
                .ok()
                .and_then(|raw| raw.parse().ok())
                .filter(|secs| *secs >= 30)
                .unwrap_or(600),
            write_mode: !env::var("MODE").is_ok_and(|v| v.trim().eq_ignore_ascii_case("read")),
            request_timeout_secs: env::var("REQUEST_TIMEOUT_SECS")
                .ok()
                .and_then(|raw| raw.parse().ok())
                .filter(|secs| *secs >= 1)
                .unwrap_or(45),
        }
    }

    /// Checks the config is actually usable: a malformed `CORS_ORIGIN` would otherwise surface as
    /// a panic deep inside the CORS layer's construction.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(origins) = &self.cors_origin {
            crate::middleware::parse_origins(origins)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(cors_origin: Option<&str>) -> Config {
        Config {
            port: "8080".to_string(),
            cors_origin: cors_origin.map(str::to_string),
            venues: vec![],
            category_aliases: String::new(),
            database_url: None,
            database_pool_size: 15,
            refresh_interval_secs: 600,
            write_mode: true,
            request_timeout_secs: 45,
        }
    }

    #[test]
    fn no_cors_origin_is_fine_and_means_any() {
        assert!(config_with(None).validate().is_ok());
    }

    #[test]
    fn a_normal_origin_validates() {
        assert!(config_with(Some("http://localhost:3000")).validate().is_ok());
    }

    #[test]
    fn several_origins_validate() {
        assert!(config_with(Some("https://metis.example, https://staging.metis.example")).validate().is_ok());
    }

    #[test]
    fn malformed_origins_are_refused_at_startup() {
        for bad in ["*", "https://*.example.com", "https://metis.example/", "https://metis.example/app", "metis.example", "http://\ninvalid"] {
            assert!(config_with(Some(bad)).validate().is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn venue_config_reads_its_own_prefixed_variables_only() {
        let vars: HashMap<String, String> = [
            ("JUPITER_API_KEY", "jk"),
            ("JUPITER_PAGE_CAP", "4"),
            ("PANTA_API_KEY", "pk"),
            ("NEW_VENUE_KIND", "jupiter"),
            ("NEW_VENUE_BASE_URL", "https://example.test/v1"),
            ("JUPITER_EMPTY", "  "),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let jupiter = VenueConfig::from_vars("jupiter", &vars);
        assert_eq!(jupiter.kind, "jupiter");
        assert_eq!(jupiter.api_key, "jk");
        assert_eq!(jupiter.number("PAGE_CAP"), Some(4));
        assert!(jupiter.option("EMPTY").is_none(), "blank values are treated as unset");

        // `<ID>_KIND` picks the adapter independently of the id.
        let other = VenueConfig::from_vars("new-venue", &vars);
        assert_eq!(other.kind, "jupiter");
        assert_eq!(other.base_url.as_deref(), Some("https://example.test/v1"));
        assert_eq!(other.api_key, "");
    }
}
