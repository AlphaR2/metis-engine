//! One error type for every route - maps internally-meaningful errors (adapter failures, routing
//! failures, bad input) onto an HTTP status + a stable JSON body, so the UI never has to parse
//! ad-hoc error shapes per endpoint.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use super::adapter_error::AdapterError;
use super::routing_error::RoutingError;

/// Every failure a route can return, mapped to one HTTP status each.
#[derive(Debug)]
pub enum ApiError {
    NotFound(String),
    BadRequest(String),
    /// A venue/upstream call failed.
    Upstream(String),
}

/// The stable JSON shape every error response takes.
#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            ApiError::Upstream(msg) => (StatusCode::BAD_GATEWAY, msg),
        };
        (status, Json(ErrorBody { error: message })).into_response()
    }
}

impl From<RoutingError> for ApiError {
    fn from(err: RoutingError) -> Self {
        match err {
            RoutingError::NoVenuesForOutcome(_) => ApiError::NotFound(err.to_string()),
            RoutingError::NoQuotesAvailable => ApiError::Upstream(err.to_string()),
        }
    }
}

impl From<AdapterError> for ApiError {
    /// Maps a venue failure onto the status that actually describes it.
    ///
    /// The distinction that matters: a 4xx is something the caller can fix by changing the
    /// request, a 5xx is something only the venue can fix. Collapsing them into one code would
    /// leave a caller retrying a malformed question forever.
    fn from(err: AdapterError) -> Self {
        match err {
            AdapterError::InvalidRequest(_) | AdapterError::Unsupported { .. } => {
                ApiError::BadRequest(err.to_string())
            }
            // The venue worked; there is simply no depth at this size. Not an outage, and not
            // something a retry fixes - the caller needs to ask for less.
            AdapterError::NoLiquidity { .. } => ApiError::BadRequest(err.to_string()),
            AdapterError::MarketNotFound(_) => ApiError::NotFound(err.to_string()),
            AdapterError::Http(_) | AdapterError::Venue(_) | AdapterError::Parse(_) => {
                ApiError::Upstream(err.to_string())
            }
        }
    }
}
