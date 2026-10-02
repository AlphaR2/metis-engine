//! Every error type in the service, in one place - the HTTP-facing `ApiError` plus the focused
//! internal error enums it wraps.

pub mod adapter_error;
pub mod api_error;
pub mod routing_error;

pub use adapter_error::AdapterError;
pub use api_error::{ApiError, ErrorBody, ErrorCode};
pub use routing_error::RoutingError;
