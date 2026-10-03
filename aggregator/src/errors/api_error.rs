//! One error type for every route - maps internally-meaningful errors (adapter failures, routing
//! failures, bad input) onto an HTTP status + a stable JSON body, so the UI never has to parse
//! ad-hoc error shapes per endpoint.
//!
//! The body carries a machine-readable [`ErrorCode`]. A client routes on the code and shows the
//! `error` text; it never has to match on message wording, which is free to change.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

use super::adapter_error::AdapterError;

/// Stable, client-routable reason for a failure. Additive only: a code is never renamed or
/// reused, because clients switch on these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// The request is malformed or a parameter is out of range. Fix the request; retrying as-is
    /// cannot help.
    InvalidRequest,
    /// The thing asked for does not exist.
    NotFound,
    /// The market does not exist on that venue.
    MarketNotFound,
    /// No venue by that id is enabled on this deployment.
    UnknownVenue,
    /// The venue does not offer this action at all (for example, buying on a secondary-phase
    /// market, or creating a market on a venue that has no such flow).
    Unsupported,
    /// Trading is switched off on this deployment.
    ExecutionDisabled,
    /// Market creation is switched off on this deployment.
    MarketCreationDisabled,
    /// The price moved past the caller's `min_shares_out` between quote and build. Re-quote.
    PriceMoved,
    /// The venue's own quote expired or its curve moved past the tolerance. Re-quote.
    QuoteStale,
    /// The venue answered, but there is no depth at this size. Ask for less.
    NoLiquidity,
    /// The amount is under the venue's minimum order.
    OrderMinSize,
    /// The market exists but cannot be traded through this venue's API right now (for example a
    /// Panta market that has left its primary phase).
    MarketNotTradable,
    /// Nothing is claimable for this position (already claimed, not won, or not settled).
    NotClaimable,
    /// The venue's window for this action closed. Rebuild.
    TransactionExpired,
    /// This client is sending too many requests. Honor `Retry-After`.
    RateLimited,
    /// The venue could not be reached or answered with an error. Usually worth retrying.
    VenueUnavailable,
}

impl ErrorCode {
    /// Whether the same request could succeed if sent again later, unchanged. `QuoteStale` and
    /// `PriceMoved` are not retryable as-is: the caller must re-quote first.
    pub fn retryable(self) -> bool {
        matches!(self, ErrorCode::RateLimited | ErrorCode::VenueUnavailable)
    }

    /// The HTTP status that describes this code when it comes from a venue's own answer.
    fn venue_status(self) -> StatusCode {
        match self {
            ErrorCode::MarketNotFound | ErrorCode::NotFound => StatusCode::NOT_FOUND,
            ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::VenueUnavailable => StatusCode::BAD_GATEWAY,
            _ => StatusCode::BAD_REQUEST,
        }
    }
}

/// Every failure a route can return, mapped to one HTTP status each.
#[derive(Debug)]
pub enum ApiError {
    NotFound(String),
    BadRequest(String),
    /// A venue/upstream call failed.
    #[allow(dead_code)]
    Upstream(String),
    /// A failure with a specific code. Prefer this wherever a client would want to act on the
    /// reason rather than just display it.
    Coded {
        status: StatusCode,
        code: ErrorCode,
        message: String,
        /// The venue's own failure code, verbatim, when it sent one (`CREATE_EXPIRED`,
        /// `TX_FEE_MISMATCH`, ...).
        venue_code: Option<String>,
    },
}

impl ApiError {
    /// A 400 with a specific code.
    pub fn bad_request(code: ErrorCode, message: impl Into<String>) -> Self {
        ApiError::Coded { status: StatusCode::BAD_REQUEST, code, message: message.into(), venue_code: None }
    }

    /// The code and text of this error, for callers that report a failure inside a larger
    /// response (one venue failing in a list) instead of failing the whole request.
    pub fn parts(self) -> (ErrorCode, String) {
        match self {
            ApiError::NotFound(message) => (ErrorCode::NotFound, message),
            ApiError::BadRequest(message) => (ErrorCode::InvalidRequest, message),
            ApiError::Upstream(message) => (ErrorCode::VenueUnavailable, message),
            ApiError::Coded { code, message, .. } => (code, message),
        }
    }

    /// A 429, for callers building their own response around `Retry-After`.
    pub fn rate_limited(message: impl Into<String>) -> Self {
        ApiError::Coded {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: ErrorCode::RateLimited,
            message: message.into(),
            venue_code: None,
        }
    }
}

/// The stable JSON shape every error response takes.
#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    /// Human-readable text, safe to show. Wording may change; route on `code`.
    pub error: String,
    pub code: ErrorCode,
    /// Whether resending the identical request later could succeed.
    pub retryable: bool,
    /// The venue's own code, verbatim, when the failure came from a venue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub venue_code: Option<String>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message, venue_code) = match self {
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, ErrorCode::NotFound, msg, None),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest, msg, None),
            ApiError::Upstream(msg) => (StatusCode::BAD_GATEWAY, ErrorCode::VenueUnavailable, msg, None),
            ApiError::Coded { status, code, message, venue_code } => (status, code, message, venue_code),
        };
        let body = ErrorBody { error: message, code, retryable: code.retryable(), venue_code };
        (status, Json(body)).into_response()
    }
}

/// The venue's own failure code inside a message, if it named one.
///
/// Both venues return SCREAMING_SNAKE names (`QUOTE_STALE`, `NOT_CLAIMABLE`, `CREATE_EXPIRED`,
/// `TX_FEE_MISMATCH`) that say far more than "it failed". This is the single place venue text is
/// read for one; it is passed through verbatim as `venue_code` and only the few we act on are
/// mapped onto our own codes.
fn venue_code_in(message: &str) -> Option<String> {
    static CODE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\b[A-Z][A-Z0-9]{1,}(?:_[A-Z0-9]+)+\b").expect("static regex")
    });
    CODE.find(message).map(|m| m.as_str().to_string())
}

/// Our code for a venue code we know how to act on.
///
/// Exact matches first: `QUOTE_EXPIRED` ends in `_EXPIRED` too, but it means "re-quote", not
/// "rebuild the transaction".
fn code_for_venue_code(venue_code: &str) -> Option<ErrorCode> {
    match venue_code {
        "QUOTE_STALE" | "QUOTE_EXPIRED" => Some(ErrorCode::QuoteStale),
        "NOT_CLAIMABLE" => Some(ErrorCode::NotClaimable),
        "AMOUNT_TOO_SMALL" => Some(ErrorCode::OrderMinSize),
        "MARKET_NOT_IN_PRIMARY" => Some(ErrorCode::MarketNotTradable),
        "MARKET_NOT_FOUND" => Some(ErrorCode::MarketNotFound),
        "INVALID_MARKET_PARAMS" => Some(ErrorCode::InvalidRequest),
        "RATE_LIMITED" => Some(ErrorCode::VenueUnavailable),
        c if c.ends_with("_EXPIRED") => Some(ErrorCode::TransactionExpired),
        _ => None,
    }
}

impl From<AdapterError> for ApiError {
    /// Maps a venue failure onto the status that actually describes it.
    ///
    /// The distinction that matters: a 4xx is something the caller can fix by changing the
    /// request, a 5xx is something only the venue can fix. Collapsing them into one code would
    /// leave a caller retrying a malformed question forever.
    fn from(err: AdapterError) -> Self {
        let message = err.to_string();
        let coded = |status, code, message: String, venue_code| ApiError::Coded { status, code, message, venue_code };
        match err {
            AdapterError::InvalidRequest(_) => {
                let venue_code = venue_code_in(&message);
                let code = venue_code.as_deref().and_then(code_for_venue_code).unwrap_or(ErrorCode::InvalidRequest);
                coded(code.venue_status(), code, message, venue_code)
            }
            AdapterError::BelowMinimum { .. } => coded(StatusCode::BAD_REQUEST, ErrorCode::OrderMinSize, message, None),
            AdapterError::Unsupported { .. } => coded(StatusCode::BAD_REQUEST, ErrorCode::Unsupported, message, None),
            // The venue worked; there is simply no depth at this size. Not an outage, and not
            // something a retry fixes - the caller needs to ask for less.
            AdapterError::NoLiquidity { .. } => coded(StatusCode::BAD_REQUEST, ErrorCode::NoLiquidity, message, None),
            AdapterError::MarketNotFound(_) => coded(StatusCode::NOT_FOUND, ErrorCode::MarketNotFound, message, None),
            AdapterError::Http(_) | AdapterError::Venue(_) | AdapterError::Parse(_) => {
                let venue_code = venue_code_in(&message);
                // A venue that named a business reason (stale quote, not claimable) is telling
                // the caller something specific, so it surfaces as that, not as an outage.
                match venue_code.as_deref().and_then(code_for_venue_code) {
                    Some(code) => coded(code.venue_status(), code, message, venue_code),
                    None => coded(StatusCode::BAD_GATEWAY, ErrorCode::VenueUnavailable, message, venue_code),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_of(err: ApiError) -> (StatusCode, serde_json::Value) {
        let response = err.into_response();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn plain_variants_keep_their_status_and_gain_a_code() {
        let (status, body) = body_of(ApiError::NotFound("no event 'x'".into())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "NOT_FOUND");
        assert_eq!(body["error"], "no event 'x'");
        assert_eq!(body["retryable"], false);

        let (status, body) = body_of(ApiError::BadRequest("bad".into())).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("INVALID_REQUEST")));

        let (status, body) = body_of(ApiError::Upstream("down".into())).await;
        assert_eq!((status, body["code"].as_str(), body["retryable"].as_bool()), (StatusCode::BAD_GATEWAY, Some("VENUE_UNAVAILABLE"), Some(true)));
    }

    #[tokio::test]
    async fn a_venue_code_is_passed_through_and_the_known_ones_are_mapped() {
        let stale: ApiError = AdapterError::InvalidRequest("panta rejected the order: QUOTE_STALE".into()).into();
        let (status, body) = body_of(stale).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "QUOTE_STALE");
        assert_eq!(body["venue_code"], "QUOTE_STALE");
        assert_eq!(body["retryable"], false, "a stale quote needs a re-quote, not a blind resend");

        let expired: ApiError = AdapterError::Venue("venue returned an error: CREATE_EXPIRED".into()).into();
        let (_, body) = body_of(expired).await;
        assert_eq!(body["code"], "TRANSACTION_EXPIRED");
        assert_eq!(body["venue_code"], "CREATE_EXPIRED");

        let unknown: ApiError = AdapterError::Venue("venue returned an error: TX_FEE_MISMATCH".into()).into();
        let (status, body) = body_of(unknown).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_GATEWAY, Some("VENUE_UNAVAILABLE")));
        assert_eq!(body["venue_code"], "TX_FEE_MISMATCH", "an unmapped venue code is still surfaced verbatim");
    }

    #[tokio::test]
    async fn venue_codes_map_to_the_action_a_client_should_take() {
        for (text, code, status) in [
            ("QUOTE_EXPIRED", "QUOTE_STALE", StatusCode::BAD_REQUEST),
            ("AMOUNT_TOO_SMALL", "ORDER_MIN_SIZE", StatusCode::BAD_REQUEST),
            ("MARKET_NOT_IN_PRIMARY", "MARKET_NOT_TRADABLE", StatusCode::BAD_REQUEST),
            ("MARKET_NOT_FOUND", "MARKET_NOT_FOUND", StatusCode::NOT_FOUND),
            ("NOT_CLAIMABLE", "NOT_CLAIMABLE", StatusCode::BAD_REQUEST),
        ] {
            let (got_status, body) = body_of(AdapterError::InvalidRequest(format!("panta: {text}")).into()).await;
            assert_eq!((got_status, body["code"].as_str()), (status, Some(code)), "{text}");
        }
    }

    #[tokio::test]
    async fn business_outcomes_are_not_reported_as_outages() {
        let (status, body) = body_of(AdapterError::NoLiquidity { venue_market_id: "m".into(), side: "yes".into() }.into()).await;
        assert_eq!((status, body["code"].as_str(), body["retryable"].as_bool()), (StatusCode::BAD_REQUEST, Some("NO_LIQUIDITY"), Some(false)));

        let (status, body) = body_of(AdapterError::Unsupported { venue: "panta", action: "claims" }.into()).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_REQUEST, Some("UNSUPPORTED")));

        let (status, body) = body_of(AdapterError::MarketNotFound("m".into()).into()).await;
        assert_eq!((status, body["code"].as_str()), (StatusCode::NOT_FOUND, Some("MARKET_NOT_FOUND")));
    }

    #[tokio::test]
    async fn a_message_without_a_venue_code_carries_none() {
        let (_, body) = body_of(AdapterError::Venue("connection reset".into()).into()).await;
        assert!(body.get("venue_code").is_none());
    }
}
