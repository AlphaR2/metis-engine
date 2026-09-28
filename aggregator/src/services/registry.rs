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
use crate::services::panta::{ChainSource, PantaAdapter};
use crate::services::pascal::PascalAdapter;

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
                if config.api_key.is_empty() && config.kind != "pascal" {
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
        "pascal" => {
            tracing::warn!("pascal enabled - its wire schema is unverified (its /markets envelope is {{status, data}}) and it reports no tradeability flag");
            Some(Arc::new(PascalAdapter::new(base_url("https://data.pascal.trade/api/v1"))))
        }
        _ => None,
    }
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
