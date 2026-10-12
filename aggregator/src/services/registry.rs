//! Builds the venue network from config. The one place that maps a configured venue `kind` to
//! its adapter; everything downstream works on `dyn VenueAdapter` plus `VenueSettings` and never
//! names a venue.
//!
//! Adding a venue: implement `VenueAdapter` under `services/<venue>/`, add one arm to
//! `build_adapter`, and enable it with `ENABLED_VENUES` plus its `<ID>_*` settings. See
//! `docs/adding-a-venue.md`.

use std::sync::Arc;
use std::time::Duration;

use crate::config::VenueConfig;
use crate::services::VenueAdapter;
use crate::services::jupiter::JupiterAdapter;
use crate::services::metis::{MetisAdapter, MetisSettings};
use crate::services::panta::{ChainSource, PantaAdapter};

/// Per-venue pacing and limits for the refresh. Every field can be set per venue from the
/// environment (`<ID>_PAGE_CAP`, …); the defaults are what both current venues were tuned to.
#[derive(Debug, Clone)]
pub struct VenueSettings {
    /// Pages fetched per pagination chain before stopping.
    ///
    /// Neither current venue can answer "what closes soonest" server-side, so there is no page at
    /// which we can prove nothing relevant remains. Each chain takes the venue's most-traded slice
    /// first and stops here - the busiest N markets per category, not completeness.
    pub page_cap: usize,
    /// Minimum gap between consecutive page requests in one chain.
    pub min_request_gap: Duration,
    /// Chains in flight against this venue at once. Jupiter 429s well below what a parallel
    /// fan-out produces, so the default is one; venues still refresh concurrently with each other.
    pub max_concurrent_chains: usize,
    /// How long refreshes skip a venue once it has failed repeatedly, before trying again.
    pub breaker_cooldown: Duration,
}

impl Default for VenueSettings {
    fn default() -> Self {
        Self {
            page_cap: 10,
            min_request_gap: Duration::from_millis(250),
            max_concurrent_chains: 1,
            breaker_cooldown: Duration::from_secs(300),
        }
    }
}

impl VenueSettings {
    /// Defaults, overridden by whatever `config` sets.
    pub fn from_config(config: &VenueConfig) -> Self {
        let defaults = Self::default();
        Self {
            page_cap: config.number("PAGE_CAP").map_or(defaults.page_cap, |n| n.max(1) as usize),
            min_request_gap: config.number("MIN_REQUEST_GAP_MS").map_or(defaults.min_request_gap, Duration::from_millis),
            max_concurrent_chains: config
                .number("MAX_CONCURRENT_CHAINS")
                .map_or(defaults.max_concurrent_chains, |n| n.max(1) as usize),
            breaker_cooldown: config.number("BREAKER_COOLDOWN_SECS").map_or(defaults.breaker_cooldown, Duration::from_secs),
        }
    }
}

/// One enabled venue: its adapter and its settings.
pub struct RegisteredVenue {
    pub adapter: Arc<dyn VenueAdapter>,
    pub settings: VenueSettings,
}

/// Builds every configured venue, skipping (with an error log) any whose `kind` is unknown.
pub fn build_venues(configs: &[VenueConfig]) -> Vec<RegisteredVenue> {
    configs
        .iter()
        .filter_map(|config| match build_adapter(config) {
            // Adapters report a fixed id today, and health, quotes and routes key on it, so a
            // configured id must match. Checked here so a mismatch fails loudly at boot.
            Some(adapter) if adapter.id() != config.id => {
                tracing::error!(
                    "venue '{}' uses adapter kind '{}', whose id is '{}' - ids must match; not enabled",
                    config.id,
                    config.kind,
                    adapter.id()
                );
                None
            }
            Some(adapter) => {
                // Metis is read from chain: there is no API behind it, so no key to be missing.
                if config.api_key.is_empty() && config.kind != "metis" {
                    tracing::warn!("{}: no API key set ({}_API_KEY) - every call will fail", config.id, config.env_prefix());
                }
                Some(RegisteredVenue { adapter, settings: VenueSettings::from_config(config) })
            }
            None => {
                tracing::error!("{}: unknown venue kind '{}' - not enabled", config.id, config.kind);
                None
            }
        })
        .collect()
}

/// The only venue-kind switch in the service.
fn build_adapter(config: &VenueConfig) -> Option<Arc<dyn VenueAdapter>> {
    let base_url = |default: &str| config.base_url.clone().unwrap_or_else(|| default.to_string());
    match config.kind.as_str() {
        "panta" => {
            // Markets are found on-chain unless `PANTA_DISCOVERY=api`: the API's list misses
            // markets (see `services/panta/chain.rs`).
            let chain = (config.option("DISCOVERY").as_deref() != Some("api")).then(|| ChainSource {
                rpc_url: config.option("RPC_URL").unwrap_or_else(|| "https://api.mainnet-beta.solana.com".to_string()),
                program_id: config.option("PROGRAM_ID").unwrap_or_else(|| crate::services::panta::chain::PROGRAM_ID.to_string()),
            });
            Some(Arc::new(PantaAdapter::new(
                base_url("https://live-api.panta.market/api/v1"),
                config.api_key.clone(),
                config.option("QUOTE_WALLET").unwrap_or_default(),
                chain,
            )))
        }
        "jupiter" => Some(Arc::new(JupiterAdapter::new(base_url("https://api.jup.ag/prediction/v1"), config.api_key.clone()))),
        "metis" => metis_adapter(config),
        _ => None,
    }
}

/// Reads a Metis venue's settings. A malformed address or amount is a boot-time error, not a venue
/// that silently points at the wrong place: `None` leaves the venue disabled with the reason logged.
fn metis_adapter(config: &VenueConfig) -> Option<Arc<dyn VenueAdapter>> {
    let program_id = match config.option("PROGRAM_ID") {
        Some(text) => text.parse().map_err(|_| tracing::error!("{}: PROGRAM_ID '{text}' is not a valid address", config.id)).ok()?,
        None => solana_pubkey::Pubkey::new_from_array(metis_core::ID.to_bytes()),
    };
    let metis_config = match config.option("CONFIG") {
        Some(text) => Some(text.parse().map_err(|_| tracing::error!("{}: CONFIG '{text}' is not a valid address", config.id)).ok()?),
        None => None,
    };
    let reference_deposit = match config.option("REFERENCE_USDC") {
        Some(text) => text
            .parse::<rust_decimal::Decimal>()
            .ok()
            .and_then(crate::services::metis::view::to_atoms)
            .filter(|atoms| *atoms > 0)
            .or_else(|| {
                tracing::error!("{}: REFERENCE_USDC '{text}' is not a positive amount with at most 6 decimals", config.id);
                None
            })?,
        None => MetisSettings::DEFAULT_REFERENCE_DEPOSIT,
    };
    Some(Arc::new(MetisAdapter::new(MetisSettings {
        rpc_url: config.option("RPC_URL").unwrap_or_else(|| "https://api.devnet.solana.com".to_string()),
        program_id,
        config: metis_config,
        reference_deposit,
    })))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(kind: &str, options: &[(&str, &str)]) -> VenueConfig {
        VenueConfig {
            id: kind.to_string(),
            kind: kind.to_string(),
            base_url: None,
            api_key: "key".to_string(),
            options: options.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>(),
        }
    }

    #[test]
    fn known_kinds_build_and_unknown_ones_are_skipped() {
        let built = build_venues(&[config("panta", &[]), config("jupiter", &[]), config("nosuchvenue", &[])]);
        let ids: Vec<&str> = built.iter().map(|v| v.adapter.id()).collect();
        assert_eq!(ids, ["panta", "jupiter"]);
    }

    #[test]
    fn metis_builds_with_defaults_and_needs_no_api_key() {
        let mut cfg = config("metis", &[]);
        cfg.api_key = String::new();
        let built = build_venues(&[cfg]);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].adapter.id(), "metis");
    }

    #[test]
    fn metis_takes_its_deployment_from_the_environment() {
        let program = solana_pubkey::Pubkey::new_unique().to_string();
        let cfg = config("metis", &[("PROGRAM_ID", &program), ("CONFIG", &program), ("REFERENCE_USDC", "25.5"), ("RPC_URL", "http://localhost:8899")]);
        assert_eq!(build_venues(&[cfg]).len(), 1);
    }

    #[test]
    fn a_malformed_metis_setting_disables_the_venue_instead_of_pointing_it_somewhere_wrong() {
        for (name, bad) in [("PROGRAM_ID", "not-an-address"), ("CONFIG", "12345"), ("REFERENCE_USDC", "ten"), ("REFERENCE_USDC", "0"), ("REFERENCE_USDC", "1.0000001")] {
            assert!(build_venues(&[config("metis", &[(name, bad)])]).is_empty(), "{name}={bad} must not build");
        }
    }

    #[test]
    fn a_configured_id_must_match_its_adapter() {
        let mut renamed = config("jupiter", &[]);
        renamed.id = "jupiter-2".to_string();
        assert!(build_venues(&[renamed]).is_empty());
    }

    #[test]
    fn settings_default_and_take_per_venue_overrides() {
        let defaults = VenueSettings::from_config(&config("jupiter", &[]));
        assert_eq!(defaults.page_cap, 10);
        let tuned = VenueSettings::from_config(&config(
            "jupiter",
            &[("PAGE_CAP", "3"), ("MIN_REQUEST_GAP_MS", "1000"), ("MAX_CONCURRENT_CHAINS", "0")],
        ));
        assert_eq!(tuned.page_cap, 3);
        assert_eq!(tuned.min_request_gap, Duration::from_millis(1000));
        assert_eq!(tuned.max_concurrent_chains, 1, "zero would deadlock the refresh, so it is clamped");
    }
}
