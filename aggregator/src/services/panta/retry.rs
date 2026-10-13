//! Retries for Panta's flaky write endpoints.
//!
//! Panta's market-create quote/build and its primary-buy quote/build intermittently answer `400` with a bare
//! `{"code":"INVALID_MARKET_PARAMS"}` (no `message`) on input that succeeded seconds earlier, and sometimes a
//! `400` whose message ends "check server logs". NectarFi's production integration measured the bare form failing
//! 50-75% of the time on open markets and rides it out with six spaced attempts. A real field error always carries
//! a `message`, so a bare one is retried; anything else is returned to the caller at once.

use std::time::Duration;

use serde::Serialize;

/// Attempts per call (the first try plus five retries).
pub const ATTEMPTS: u32 = 6;

/// Panta's transient-failure tells: a message ending "check server logs", or `INVALID_MARKET_PARAMS` with no message.
pub fn is_flaky_body(text: &str) -> bool {
    if text.contains("check server logs") {
        return true;
    }
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .is_some_and(|v| v.get("code").and_then(|c| c.as_str()) == Some("INVALID_MARKET_PARAMS") && v.get("message").is_none())
}

fn backoff(attempt: u32) -> Duration {
    // 0.5s, 1s, 1.5s, 2s, 2.5s: about 7.5 seconds across six attempts.
    Duration::from_millis((u64::from(attempt) * 500).min(2_500))
}

/// `POST`s JSON with the API key, retrying 5xx and Panta's flaky 400. Hands back a response the caller checks as usual:
/// the first success, or the last failure once the attempts run out.
pub async fn post_json<B: Serialize + ?Sized>(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: &B,
) -> Result<reqwest::Response, reqwest::Error> {
    let mut attempt = 1;
    loop {
        let sent = client.post(url).header("X-Api-Key", api_key).json(body).send().await;
        let last = attempt >= ATTEMPTS;
        let response = match sent {
            Ok(r) => r,
            Err(e) if last => return Err(e),
            Err(_) => {
                tokio::time::sleep(backoff(attempt)).await;
                attempt += 1;
                continue;
            }
        };
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let text = response.text().await.unwrap_or_default();
        if !last && (status.is_server_error() || is_flaky_body(&text)) {
            tracing::warn!(url, attempt, %status, "panta transient failure, retrying");
            tokio::time::sleep(backoff(attempt)).await;
            attempt += 1;
            continue;
        }
        return Ok(reqwest::Response::from(axum::http::Response::builder().status(status).body(text).expect("a status and a string body")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_invalid_market_params_is_the_flake_but_a_real_field_error_is_not() {
        assert!(is_flaky_body(r#"{"code":"INVALID_MARKET_PARAMS"}"#));
        assert!(is_flaky_body(r#"{"code":"X","message":"something, check server logs"}"#));
        assert!(!is_flaky_body(r#"{"code":"INVALID_MARKET_PARAMS","message":"quoteId: This field is required."}"#));
        assert!(!is_flaky_body(r#"{"code":"DUPLICATE_MARKET"}"#));
        assert!(!is_flaky_body("not json"));
    }

    #[test]
    fn the_wait_grows_then_levels_off() {
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(5), Duration::from_millis(2_500));
        assert_eq!(backoff(9), Duration::from_millis(2_500));
    }
}
