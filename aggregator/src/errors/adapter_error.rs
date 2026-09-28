use std::error::Error as StdError;
use std::fmt;

use thiserror::Error;

/// reqwest's own `Display` for a transport-level failure is deliberately shallow (e.g. "error
/// sending request for url (...)") - the actually useful detail (DNS failure, TLS handshake
/// rejection, connection reset, timeout) lives in its `source()` chain. Walking it here means a
/// failure shows up in `/venues/health`'s `last_error` with enough detail to actually diagnose,
/// instead of a generic message that hides what really went wrong.
struct ReqwestErrorChain<'a>(&'a reqwest::Error);

impl fmt::Display for ReqwestErrorChain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;
        let mut source: Option<&dyn StdError> = self.0.source();
        while let Some(err) = source {
            write!(f, " -> {err}")?;
            source = err.source();
        }
        Ok(())
    }
}

/// Everything that can go wrong calling out to a venue's API.
#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("http request failed: {}", ReqwestErrorChain(.0))]
    Http(#[from] reqwest::Error),
    #[error("venue returned an error: {0}")]
    Venue(String),
    #[error("could not parse venue response: {0}")]
    Parse(String),
    #[error("market not found: {0}")]
    MarketNotFound(String),
    /// The caller's own parameters are wrong - an over-long question, times out of order, a
    /// category this venue doesn't have. Distinct from `Venue`, which means the venue itself
    /// failed: this is a 400 the caller can fix, not a 502 they can only retry.
    #[error("{0}")]
    InvalidRequest(String),
    /// This venue does not offer the action at all - Panta cannot be traded on the secondary
    /// market, Jupiter cannot create markets. Distinct from a failure: nothing went wrong, the
    /// capability simply isn't there, and a caller should be told which venue and which action
    /// rather than seeing a generic error.
    #[error("{venue} does not support {action}")]
    Unsupported { venue: &'static str, action: &'static str },
    /// The venue answered fine, the market just has no depth on this side right now. A *business*
    /// outcome, not an outage: it must never count against `HealthRegistry`, or a run of thin
    /// markets takes a perfectly healthy venue out of routing entirely.
    #[error("no liquidity for {venue_market_id} side {side}")]
    NoLiquidity { venue_market_id: String, side: String },
}

impl AdapterError {
    /// True when the venue answered correctly and the answer was simply "not available" - a fact
    /// about the market, not evidence the venue is unhealthy.
    ///
    /// `HealthRegistry` drops a venue from routing after a few consecutive failures, so counting
    /// these would let a run of thin or missing markets take a perfectly working venue offline.
    /// True when the venue rejected our credentials. Retrying cannot fix this - the key is
    /// missing, wrong, or unauthorized for the endpoint - so the refresh fails fast instead of
    /// spending three attempts and several seconds of backoff per page on a certainty.
    pub fn is_auth_failure(&self) -> bool {
        match self {
            AdapterError::Venue(msg) => msg.contains("401") || msg.contains("403"),
            AdapterError::Http(err) => err.status().is_some_and(|s| matches!(s.as_u16(), 401 | 403)),
            _ => false,
        }
    }

    /// True when the venue turned us away for asking too often. Distinguished from a generic
    /// venue error so the refresh can back off properly instead of retrying straight into
    /// another rejection.
    pub fn is_rate_limited(&self) -> bool {
        match self {
            AdapterError::Venue(msg) => msg.contains("429"),
            AdapterError::Http(err) => err.status().is_some_and(|s| s.as_u16() == 429),
            _ => false,
        }
    }

    /// True when the venue refused the request itself as malformed or unacceptable (a 4xx other
    /// than auth, not-found and rate-limit) - e.g. a category filter it doesn't accept. Retrying
    /// can't help, and it is not evidence the venue is down.
    pub fn is_client_rejection(&self) -> bool {
        let rejected = |code: u16| (400..500).contains(&code) && !matches!(code, 401 | 403 | 404 | 429);
        match self {
            AdapterError::Http(err) => err.status().is_some_and(|s| rejected(s.as_u16())),
            // `error_for_status` errors are carried as text: "HTTP status client error (400 Bad
            // Request) for url (...)".
            AdapterError::Venue(msg) => msg
                .split("client error (")
                .nth(1)
                .and_then(|rest| rest.get(..3))
                .and_then(|code| code.parse().ok())
                .is_some_and(rejected),
            AdapterError::InvalidRequest(_) => true,
            _ => false,
        }
    }

    pub fn is_business_outcome(&self) -> bool {
        matches!(
            self,
            AdapterError::NoLiquidity { .. }
                | AdapterError::MarketNotFound(_)
                | AdapterError::Unsupported { .. }
                | AdapterError::InvalidRequest(_)
        )
    }
}
