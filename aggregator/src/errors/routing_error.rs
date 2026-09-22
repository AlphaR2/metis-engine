use thiserror::Error;

/// Why `find_best_market` couldn't produce a recommendation.
#[derive(Debug, Error)]
pub enum RoutingError {
    #[error("no venue markets found for outcome '{0}'")]
    NoVenuesForOutcome(String),
    #[error("no venue could produce a quote")]
    NoQuotesAvailable,
}
