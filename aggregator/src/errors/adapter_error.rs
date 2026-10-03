use std::error::Error as StdError;
use std::fmt;

use thiserror::Error;

/// reqwest's own `Display` for a transport-level failure is deliberately shallow (e.g. "error
/// sending request for url (...)") - the actually useful detail (DNS failure, TLS handshake
/// rejection, connection reset, timeout) lives in its `source()` chain. Walking it here means a
/// failure shows up in `/venues/health`'s `last_error` with enough detail to actually diagnose,
/// instead of a generic message that hides what really went wrong.
struct ReqwestErrorChain<'a>(&'a reqwest::Error);

/// Replaces every URL in `text` with `<url>`.
///
/// Venue and RPC errors embed the request URL, and a private RPC endpoint carries its API key in
/// that URL (query, path or subdomain depending on the provider). These messages reach
/// `/venues/health`, API error bodies and the logs, so the URL is stripped here, at the one
/// conversion every path passes through, rather than trusting each call site to remember.
pub fn redact_urls(text: &str) -> String {
    static URL: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r#"(?i)\b[a-z][a-z0-9+.-]*://[^\s)"'<>]+"#).expect("static regex"));
    URL.replace_all(text, "<url>").into_owned()
}

impl fmt::Display for ReqwestErrorChain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text = self.0.to_string();
        let mut source: Option<&dyn StdError> = self.0.source();
        while let Some(err) = source {
            text.push_str(&format!(" -> {err}"));
            source = err.source();
        }
        write!(f, "{}", redact_urls(&text))
    }
}

/// Everything that can go wrong calling out to a venue's API.
#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("http request failed: {}", ReqwestErrorChain(.0))]
    Http(#[from] reqwest::Error),
    #[error("venue returned an error: {}", redact_urls(.0))]
    Venue(String),
    #[error("could not parse venue response: {}", redact_urls(.0))]
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
    #[allow(dead_code)]
    NoLiquidity { venue_market_id: String, side: String },
    /// The amount is under the venue's minimum order. Caught before any venue call, so the
    /// caller is told plainly instead of being charged for a build that could never work.
    #[error("{venue}'s minimum order is ${minimum_usdc}; this one is ${amount_usdc}")]
    BelowMinimum { venue: &'static str, minimum_usdc: String, amount_usdc: String },
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
                | AdapterError::BelowMinimum { .. }
                | AdapterError::MarketNotFound(_)
                | AdapterError::Unsupported { .. }
                | AdapterError::InvalidRequest(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_carrying_keys_are_stripped_wherever_the_key_sits() {
        for url in [
            "https://rpc.example.com/?api-key=SECRET",
            "https://SECRET.quiknode.pro/abc123/",
            "https://user:SECRET@rpc.example.com/v1",
        ] {
            let shown = AdapterError::Venue(format!("panta rpc: error sending request for url ({url})")).to_string();
            assert!(!shown.contains("SECRET"), "{shown}");
            assert!(shown.contains("<url>"), "{shown}");
        }
    }

    #[test]
    fn text_without_a_url_is_untouched() {
        assert_eq!(redact_urls("HTTP status 401"), "HTTP status 401");
    }

    #[test]
    fn parse_errors_are_redacted_too() {
        let shown = AdapterError::Parse("bad body from http://h/x?key=SECRET".into()).to_string();
        assert!(!shown.contains("SECRET"));
    }

    #[test]
    fn auth_detection_still_sees_the_raw_status() {
        assert!(AdapterError::Venue("401 for url (https://h/x?key=S)".into()).is_auth_failure());
    }
}
