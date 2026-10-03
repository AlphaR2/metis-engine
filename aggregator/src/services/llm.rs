//! A minimal Anthropic Messages client for the model-assisted event grouping (`engine::ai`).
//!
//! Optional by design: with no `ANTHROPIC_API_KEY` there is no client and the rules alone build the
//! events. Any failure here is returned to the caller, which treats it as "no hints this time".

use std::time::Duration;

use serde_json::json;

/// The model used for grouping: small and fast, since the answer is a short JSON list.
pub const MODEL: &str = "claude-haiku-4-5-20251001";

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const VERSION: &str = "2023-06-01";

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    api_key: String,
}

impl LlmClient {
    /// A client when `ANTHROPIC_API_KEY` is set to something.
    pub fn from_env() -> Option<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY").ok()?;
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        let http = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().ok()?;
        Some(Self { http, api_key: key.to_string() })
    }

    /// Sends one prompt and returns the model's text. Errors are plain strings: the key is never
    /// part of one.
    pub async fn complete(&self, system: &str, user: &str) -> Result<String, String> {
        let body = json!({
            "model": MODEL,
            "max_tokens": 4096,
            "system": system,
            "messages": [{ "role": "user", "content": user }],
        });
        let response = self
            .http
            .post(ENDPOINT)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("could not reach the model: {}", e.without_url()))?;
        let status = response.status();
        let value: serde_json::Value = response.json().await.map_err(|_| "the model sent an unreadable reply".to_string())?;
        if !status.is_success() {
            let detail = value["error"]["message"].as_str().unwrap_or("no detail");
            return Err(format!("the model answered {status}: {detail}"));
        }
        value["content"]
            .as_array()
            .and_then(|parts| parts.iter().find_map(|p| p["text"].as_str()))
            .map(str::to_string)
            .ok_or_else(|| "the model's reply had no text".to_string())
    }
}
