//! One place that declares every env var this service reads - `Config::from_env()` builds it,
//! `.validate()` checks it's actually usable before the server starts.

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
    /// Empty when unset - every call to that venue then fails, logged as a warning at boot.
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
    pub cors_origin: String,
    /// The venues to wire up, in `ENABLED_VENUES` order (comma-separated ids, default
    /// `panta,jupiter`). Each reads its own `<ID>_*` settings; see `VenueConfig`.
    pub venues: Vec<VenueConfig>,
    /// Extra category aliases, `from=to,from=to`, on top of `CategoryAliases::defaults()`.
    pub category_aliases: String,
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
    /// Longest any single API request may run before it is answered 408. Default 45s: long
    /// enough for a `quote_size` request's concurrent live quotes (each capped at 30s by the
    /// venue client), short enough that a stuck venue can't pin connections open.
    pub request_timeout_secs: u64,
    /// Requests per minute each client IP may make to the public routes; a request that takes live
    /// venue quotes costs 10. `0` disables limiting. Default 300.
    pub rate_limit_per_minute: u32,
    /// Take the client IP from the first `X-Forwarded-For` entry. Only safe behind a proxy that
    /// overwrites that header - otherwise any client can pick its own identity. Default false.
    pub trust_forwarded_for: bool,
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
        let vars: HashMap<String, String> = env::vars().collect();
        let enabled_venues: Vec<String> = env::var("ENABLED_VENUES")
            .ok()
            .map(|raw| raw.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_else(default_enabled_venues);
        Self {
            port: env::var("PORT").unwrap_or_else(|_| "8080".to_string()),
            cors_origin: env::var("CORS_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".to_string()),
            venues: enabled_venues.iter().map(|id| VenueConfig::from_vars(id, &vars)).collect(),
            category_aliases: env::var("CATEGORY_ALIASES").unwrap_or_default(),
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
            request_timeout_secs: env::var("REQUEST_TIMEOUT_SECS")
                .ok()
                .and_then(|raw| raw.parse().ok())
                .filter(|secs| *secs >= 1)
                .unwrap_or(45),
            rate_limit_per_minute: env::var("RATE_LIMIT_PER_MINUTE")
                .ok()
                .and_then(|raw| raw.parse().ok())
                .unwrap_or(300),
            trust_forwarded_for: env::var("TRUST_FORWARDED_FOR")
                .map(|v| v.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(false),
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
            venues: vec![],
            category_aliases: String::new(),
            database_url: None,
            database_pool_size: 15,
            refresh_interval_secs: 600,
            execution_enabled: false,
            market_creation_enabled: false,
            admin_token: None,
            request_timeout_secs: 45,
            rate_limit_per_minute: 300,
            trust_forwarded_for: false,
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
