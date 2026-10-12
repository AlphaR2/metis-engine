//! Sports-data providers. Each one turns its own response into the oracle's `Observation`; the
//! rest of the keeper never sees a provider's wire format.
//!
//! To add a provider: implement `ScoreSource` in a new file here, write its `parse` as a pure
//! function over `serde_json::Value`, and test it against a captured response.

use async_trait::async_trait;

use crate::model::Observation;

pub mod api_football;
pub mod football_data;
pub mod scripted;

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("http: {0}")]
    Http(String),
    #[error("unexpected response: {0}")]
    Parse(&'static str),
    /// Postponed, abandoned, awarded, suspended... A match the keeper must not walk through the
    /// normal phases; a human decides (wait, or void through the two-root override).
    #[error("abnormal match status: {0}")]
    Abnormal(String),
}

#[async_trait]
pub trait ScoreSource: Send + Sync {
    /// Short, stable name; also the provider part of the fixture's Merkle leaf.
    fn id(&self) -> &'static str;
    /// What the provider says about its match `external_id` right now.
    async fn observe(&self, external_id: &str) -> Result<Observation, SourceError>;
}

/// Reads `v[a][b]...` as a u8 goal count, treating missing or null as `None`.
pub(crate) fn goals(v: &serde_json::Value, home: &[&str], away: &[&str]) -> Option<(u8, u8)> {
    let at = |path: &[&str]| path.iter().try_fold(v, |cur, key| cur.get(key)).and_then(|x| x.as_u64());
    let (h, a) = (at(home)?, at(away)?);
    // A score above 15 can't be packed into the oracle's nibbles; refuse rather than clamp.
    (h <= 15 && a <= 15).then_some((h as u8, a as u8))
}
