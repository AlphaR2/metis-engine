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

/// Turns a non-success venue response into the right [`AdapterError`], keeping the venue's own
/// words.
///
/// `error_for_status()` throws the body away, and the body is where both venues say what went wrong
/// (`NOT_CLAIMABLE`, `QUOTE_STALE`, `AMOUNT_TOO_SMALL`). A 400, 409 or 422 is the caller's request
/// being refused, so it becomes `InvalidRequest` carrying that text; anything else is the venue
/// failing, and stays `Venue`. URLs are redacted, since a body can echo a request URL.
pub async fn check_status(response: reqwest::Response, context: &str) -> Result<reqwest::Response, crate::errors::AdapterError> {
    use crate::errors::AdapterError;
    use crate::errors::adapter_error::redact_urls;

    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    let snippet: String = redact_urls(&body).chars().take(300).collect();
    match status.as_u16() {
        400 | 409 | 422 => Err(AdapterError::InvalidRequest(format!("{context}: {snippet}"))),
        code => Err(AdapterError::Venue(format!("{context}: HTTP {code} - {snippet}"))),
    }
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

/// Builds `{base_url}/{segments...}` with every segment percent-escaped by `Url`, so a
/// caller-supplied id can never add path structure: `/` inside a segment becomes `%2F`, and a
/// segment that is exactly `.` or `..` is refused outright (`Url` would otherwise resolve it and
/// walk out of the venue's API prefix).
///
/// Use this for any URL that contains an id or path the venue did not hand us itself.
pub fn venue_url(base_url: &str, segments: &[&str]) -> Result<reqwest::Url, crate::errors::AdapterError> {
    use crate::errors::AdapterError;

    let mut url = reqwest::Url::parse(base_url)
        .map_err(|e| AdapterError::InvalidRequest(format!("bad venue base url '{base_url}': {e}")))?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| AdapterError::InvalidRequest(format!("venue base url '{base_url}' cannot take a path")))?;
        path.pop_if_empty();
        for segment in segments {
            if segment.is_empty() || *segment == "." || *segment == ".." {
                return Err(AdapterError::InvalidRequest(format!("invalid path segment '{segment}'")));
            }
            path.push(segment);
        }
    }
    Ok(url)
}

/// `venue_url` with a trailing slash, for APIs (Panta) whose routes require one.
pub fn venue_url_dir(base_url: &str, segments: &[&str]) -> Result<reqwest::Url, crate::errors::AdapterError> {
    let mut url = venue_url(base_url, segments)?;
    url.path_segments_mut().expect("venue_url already validated the base").push("");
    Ok(url)
}

/// Splits a caller-supplied relative path into segments for `venue_url`, keeping a trailing
/// slash (Panta's routes need one) as a final empty segment.
///
/// Rejects dot segments and backslashes rather than normalising them: nothing legitimate needs
/// them, and silently resolving `a/../b` is exactly how a traversal gets through.
pub fn split_relative_path(path: &str) -> Result<(Vec<&str>, bool), crate::errors::AdapterError> {
    use crate::errors::AdapterError;

    let trimmed = path.trim_start_matches('/');
    let trailing_slash = trimmed.ends_with('/');
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err(AdapterError::InvalidRequest("empty path".to_string()));
    }
    if segments.iter().any(|s| *s == "." || *s == ".." || s.contains('\\')) {
        return Err(AdapterError::InvalidRequest(format!("path '{path}' contains a dot segment or backslash")));
    }
    Ok((segments, trailing_slash))
}

/// `venue_url` for a caller-supplied relative path, preserving a trailing slash.
pub fn venue_url_for_path(base_url: &str, path: &str) -> Result<reqwest::Url, crate::errors::AdapterError> {
    let (segments, trailing_slash) = split_relative_path(path)?;
    let mut url = venue_url(base_url, &segments)?;
    if trailing_slash {
        url.path_segments_mut().expect("already has a path").push("");
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://api.jup.ag/prediction/v1";

    #[test]
    fn segments_are_appended_to_the_base_path() {
        assert_eq!(venue_url(BASE, &["markets", "POLY-1"]).unwrap().as_str(), "https://api.jup.ag/prediction/v1/markets/POLY-1");
    }

    #[test]
    fn a_slash_inside_an_id_is_escaped_not_followed() {
        let url = venue_url(BASE, &["markets", "../../swap"]).unwrap();
        assert_eq!(url.as_str(), "https://api.jup.ag/prediction/v1/markets/..%2F..%2Fswap");
    }

    #[test]
    fn dir_urls_keep_the_trailing_slash_panta_needs() {
        assert_eq!(
            venue_url_dir("https://live-api.panta.market/api/v1", &["markets", "9xF"]).unwrap().as_str(),
            "https://live-api.panta.market/api/v1/markets/9xF/"
        );
    }

    #[test]
    fn a_dot_dot_id_is_rejected() {
        assert!(venue_url(BASE, &["orders", "status", ".."]).is_err());
        assert!(venue_url(BASE, &["markets", ""]).is_err());
    }

    #[test]
    fn a_trailing_slash_base_does_not_double_up() {
        assert_eq!(venue_url("https://x.io/api/v1/", &["markets"]).unwrap().as_str(), "https://x.io/api/v1/markets");
    }

    #[test]
    fn relative_paths_reject_traversal_and_keep_trailing_slashes() {
        assert!(venue_url_for_path(BASE, "../swap/v1/quote").is_err());
        assert!(venue_url_for_path(BASE, "events/../../swap").is_err());
        assert!(venue_url_for_path(BASE, "a\\..\\b").is_err());
        assert!(venue_url_for_path(BASE, "").is_err());
        assert_eq!(
            venue_url_for_path("https://live-api.panta.market/api/v1", "markets/").unwrap().as_str(),
            "https://live-api.panta.market/api/v1/markets/"
        );
    }
}
