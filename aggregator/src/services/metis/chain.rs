//! Reading the Metis program straight from a Solana RPC node.
//!
//! There is no Metis API: the program's accounts ARE the catalog. Markets are found with one
//! `getProgramAccounts` call per account type, filtered by size and discriminator, and decoded
//! with the program's own types (`metis_core` is linked as a library), so the adapter can never
//! read a layout the program doesn't write.

use std::collections::HashMap;
use std::mem::{offset_of, size_of};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use metis_core::amm::{
    metadata::{MARKET_METADATA_DISCRIMINATOR, MarketMetadata},
    state::{MARKET_DISCRIMINATOR, Market},
};
use serde_json::{Value, json};
use solana_pubkey::Pubkey;

use crate::errors::AdapterError;

pub struct Rpc {
    client: reqwest::Client,
    url: String,
}

/// What the chain says about a transaction signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureState {
    /// Not seen (yet, or never).
    Unknown,
    Processed,
    Landed,
    Failed(String),
}

impl Rpc {
    pub fn new(client: reqwest::Client, url: String) -> Self {
        Self { client, url }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, AdapterError> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let response = self
            .client
            .post(&self.url)
            .json(&body)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| AdapterError::Venue(format!("metis rpc {method}: {e}")))?;
        let mut response: Value =
            response.json().await.map_err(|e| AdapterError::Parse(format!("metis rpc {method}: {e}")))?;
        if let Some(error) = response.get("error") {
            return Err(AdapterError::Venue(format!("metis rpc {method}: {error}")));
        }
        response
            .get_mut("result")
            .map(Value::take)
            .ok_or_else(|| AdapterError::Parse(format!("metis rpc {method}: no result")))
    }

    /// Every account of one type the program owns: `(address, raw data)`.
    async fn accounts_where(&self, program: &Pubkey, size: usize, disc_offset: usize, disc: &[u8; 8]) -> Result<Vec<(Pubkey, Vec<u8>)>, AdapterError> {
        let result = self
            .call(
                "getProgramAccounts",
                json!([program.to_string(), {
                    "encoding": "base64",
                    "filters": [
                        { "dataSize": size },
                        { "memcmp": { "offset": disc_offset, "bytes": BASE64.encode(disc), "encoding": "base64" } },
                    ],
                }]),
            )
            .await?;
        let rows = result.as_array().ok_or_else(|| AdapterError::Parse("metis rpc: getProgramAccounts was not a list".into()))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let address = row.get("pubkey").and_then(Value::as_str).and_then(|s| s.parse::<Pubkey>().ok());
            let data = row
                .pointer("/account/data/0")
                .and_then(Value::as_str)
                .and_then(|s| BASE64.decode(s).ok());
            match (address, data) {
                (Some(a), Some(d)) => out.push((a, d)),
                _ => tracing::warn!("metis: skipped an account that could not be read"),
            }
        }
        Ok(out)
    }

    /// Every market the program holds.
    pub async fn markets(&self, program: &Pubkey) -> Result<Vec<(Pubkey, Market)>, AdapterError> {
        let rows = self
            .accounts_where(program, size_of::<Market>(), offset_of!(Market, discriminator), &MARKET_DISCRIMINATOR)
            .await?;
        Ok(rows.into_iter().filter_map(|(a, d)| decode_market(&d).map(|m| (a, m))).collect())
    }

    /// Every market's text, keyed by the market it describes. A market without any yet is simply
    /// absent from the map.
    pub async fn metadata(&self, program: &Pubkey) -> Result<HashMap<Pubkey, MarketMetadata>, AdapterError> {
        let rows = self.accounts_where(program, MarketMetadata::LEN, 0, &MARKET_METADATA_DISCRIMINATOR).await?;
        Ok(rows
            .into_iter()
            .filter_map(|(_, d)| decode_metadata(&d))
            .map(|m| (Pubkey::new_from_array(m.market.to_bytes()), m))
            .collect())
    }

    /// One account's data, or `None` if it doesn't exist.
    pub async fn account(&self, address: &Pubkey) -> Result<Option<Vec<u8>>, AdapterError> {
        let result = self.call("getAccountInfo", json!([address.to_string(), { "encoding": "base64" }])).await?;
        Ok(result
            .pointer("/value/data/0")
            .and_then(Value::as_str)
            .map(|s| BASE64.decode(s))
            .transpose()
            .map_err(|e| AdapterError::Parse(format!("metis rpc getAccountInfo: {e}")))?)
    }

    /// Several accounts at once; `None` for any that don't exist. Same order as `addresses`.
    pub async fn accounts(&self, addresses: &[Pubkey]) -> Result<Vec<Option<Vec<u8>>>, AdapterError> {
        if addresses.is_empty() {
            return Ok(Vec::new());
        }
        let keys: Vec<String> = addresses.iter().map(Pubkey::to_string).collect();
        let result = self.call("getMultipleAccounts", json!([keys, { "encoding": "base64" }])).await?;
        let rows = result
            .pointer("/value")
            .and_then(Value::as_array)
            .ok_or_else(|| AdapterError::Parse("metis rpc: getMultipleAccounts had no value list".into()))?;
        Ok(rows.iter().map(|row| row.pointer("/data/0").and_then(Value::as_str).and_then(|s| BASE64.decode(s).ok())).collect())
    }

    /// The current slot, which order expiries are measured against.
    pub async fn slot(&self) -> Result<u64, AdapterError> {
        self.call("getSlot", json!([{ "commitment": "confirmed" }]))
            .await?
            .as_u64()
            .ok_or_else(|| AdapterError::Parse("metis rpc: getSlot was not a number".into()))
    }

    /// Every classic SPL token balance a wallet holds, by mint. Zero balances are dropped.
    pub async fn token_balances(&self, owner: &Pubkey) -> Result<HashMap<Pubkey, u64>, AdapterError> {
        let result = self
            .call(
                "getTokenAccountsByOwner",
                json!([owner.to_string(), { "programId": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA" }, { "encoding": "jsonParsed" }]),
            )
            .await?;
        let rows = result
            .pointer("/value")
            .and_then(Value::as_array)
            .ok_or_else(|| AdapterError::Parse("metis rpc: getTokenAccountsByOwner had no value list".into()))?;
        let mut balances: HashMap<Pubkey, u64> = HashMap::new();
        for row in rows {
            let info = row.pointer("/account/data/parsed/info");
            let mint = info.and_then(|i| i.get("mint")).and_then(Value::as_str).and_then(|s| s.parse::<Pubkey>().ok());
            let amount = info
                .and_then(|i| i.pointer("/tokenAmount/amount"))
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<u64>().ok());
            if let (Some(mint), Some(amount)) = (mint, amount) {
                if amount > 0 {
                    *balances.entry(mint).or_default() += amount;
                }
            }
        }
        Ok(balances)
    }

    /// The latest blockhash and the last block height at which it is still valid.
    pub async fn latest_blockhash(&self) -> Result<([u8; 32], u64), AdapterError> {
        let result = self.call("getLatestBlockhash", json!([{ "commitment": "confirmed" }])).await?;
        let hash = result
            .pointer("/value/blockhash")
            .and_then(Value::as_str)
            .ok_or_else(|| AdapterError::Parse("metis rpc: no blockhash".into()))?;
        let height = result
            .pointer("/value/lastValidBlockHeight")
            .and_then(Value::as_u64)
            .ok_or_else(|| AdapterError::Parse("metis rpc: no lastValidBlockHeight".into()))?;
        let bytes = bs58::decode(hash).into_vec().map_err(|_| AdapterError::Parse("metis rpc: blockhash is not base58".into()))?;
        let hash = <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| AdapterError::Parse("metis rpc: blockhash is not 32 bytes".into()))?;
        Ok((hash, height))
    }

    pub async fn signature_state(&self, signature: &str) -> Result<SignatureState, AdapterError> {
        let result = self
            .call("getSignatureStatuses", json!([[signature], { "searchTransactionHistory": true }]))
            .await?;
        let Some(status) = result.pointer("/value/0").filter(|v| !v.is_null()) else {
            return Ok(SignatureState::Unknown);
        };
        if let Some(err) = status.get("err").filter(|e| !e.is_null()) {
            return Ok(SignatureState::Failed(err.to_string()));
        }
        Ok(match status.get("confirmationStatus").and_then(Value::as_str) {
            Some("confirmed" | "finalized") => SignatureState::Landed,
            _ => SignatureState::Processed,
        })
    }
}

/// A market account, if `data` really is one. `pod_read_unaligned`: RPC data arrives in a plain
/// byte buffer with no alignment promise.
pub fn decode_market(data: &[u8]) -> Option<Market> {
    if data.len() != size_of::<Market>() {
        return None;
    }
    let market: Market = bytemuck::pod_read_unaligned(data);
    (market.discriminator == MARKET_DISCRIMINATOR).then_some(market)
}

pub fn decode_metadata(data: &[u8]) -> Option<MarketMetadata> {
    if data.len() != MarketMetadata::LEN {
        return None;
    }
    let metadata: MarketMetadata = bytemuck::pod_read_unaligned(data);
    (metadata.discriminator == MARKET_METADATA_DISCRIMINATOR).then_some(metadata)
}

/// The balance held by an SPL token account's raw data (`amount` is at bytes 64..72).
pub fn token_amount(data: &[u8]) -> u64 {
    data.get(64..72).and_then(|b| <[u8; 8]>::try_from(b).ok()).map(u64::from_le_bytes).unwrap_or(0)
}
