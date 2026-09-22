//! Shared HTTP client construction. `reqwest::Client::new()` has no timeout by default - a slow
//! or unreachable venue would then hang a caller (including the boot-time `refresh()` call)
//! forever with no error, no log line, nothing. Every adapter builds its client through here
//! instead, so that can't happen silently again.

use std::time::Duration;

use serde::de::DeserializeOwned;

/// How long any single request to a venue may take before it's treated as a failure. 10s was too
/// tight in practice - Jupiter and Panta list pages intermittently take longer and were timing
/// out mid-refresh (paired with per-page retries in `Aggregator`).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A `reqwest::Client` with a sane request timeout - use this instead of `Client::new()` for any
/// outbound call to a venue.
pub fn client_with_timeout() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("reqwest client with a timeout should always build")
}

/// How much of a mismatched response body to keep in a diagnostic error - enough to actually see
/// what's wrong, not so much it floods `/venues/health`'s `last_error`.
const BODY_SNIPPET_LEN: usize = 500;

/// Parses a response body as JSON, but on failure captures a snippet of the *actual* raw body in
/// the error instead of just serde's type-mismatch message. A bare "invalid type: expected a
/// string" tells you nothing about *what* the venue actually sent; the raw body is what actually
/// explains a real schema drift (confirmed necessary this session - Jupiter's response didn't
/// match our assumed shape, and without this we had no way to see why).
pub async fn parse_json_with_diagnostics<T: DeserializeOwned>(
    response: reqwest::Response,
    context: &str,
) -> Result<T, String> {
    let text = response.text().await.map_err(|e| format!("{context}: could not read response body: {e}"))?;
    serde_json::from_str(&text).map_err(|e| {
        let snippet: String = text.chars().take(BODY_SNIPPET_LEN).collect();
        format!("{context}: response didn't match the expected schema: {e} - body snippet: {snippet}")
    })
}
