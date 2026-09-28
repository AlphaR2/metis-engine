//! Panta's markets read straight from its Solana program - the discovery half of the adapter.
//!
//! Panta's `GET /markets/` lists its *registry*, not a chain scan - some live markets simply
//! aren't in it. On 2026-09-24 the program held 191 markets on mainnet and the list reached 99 of
//! them, missing 5 of the 8 still open - including the busiest. (Its cursor pagination was also
//! broken then - stuck on page 1 - but that was fixed venue-side on 2026-09-28 and is no longer
//! why markets go missing; the registry gap is the real, still-open reason discovery reads the
//! chain directly.) Every market is an `Event` account on the program, so one `getProgramAccounts`
//! call finds them all; the API is then asked about each open one by id.
//!
//! The program publishes no IDL. The layout below was mapped from live accounts against the API's
//! own values for the same markets (`docs/venue-fields.md`), and `decode_event` is tested against
//! captured accounts. A layout it does not recognise yields no phase, and the caller falls back to
//! the API's.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::errors::AdapterError;

/// Panta's program on mainnet.
pub const PROGRAM_ID: &str = "6gM5afTQBq5VZCfgpGqcsqzfWd5maLSCKWtGjbEobZMp";

/// Anchor account discriminator of `Event`: `sha256("account:Event")[..8]`.
const EVENT_DISCRIMINATOR: [u8; 8] = [0x7d, 0xc0, 0x7d, 0x9e, 0x09, 0x73, 0x98, 0xe9];

// Fixed header offsets.
const START_TIME: usize = 40;
const END_TIME: usize = 48;
const RESOLVED_AT: usize = 72;
const CANCELLED_AT: usize = 80;
/// YES price of the last trade, 1e9 fixed point.
const LAST_YES_PRICE: usize = 152;
/// Lifetime traded volume, USDC base units (1e6).
const TOTAL_VOLUME: usize = 168;
/// Question, then rules (u32-length-prefixed strings), then sources (`Vec<String>`).
const TEXT: usize = 328;

/// Where `primaryPhaseEndTime` sits after the text, by account size - the program has migrated
/// its `Event` layout once (`MigrateEventV2`), and both sizes are live.
fn primary_phase_end_offset(account_len: usize) -> Option<usize> {
    match account_len {
        2947 => Some(28),
        2982 => Some(37),
        _ => None,
    }
}

/// One `Event` account, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainEvent {
    pub market_id: String,
    pub question: String,
    pub rules: String,
    pub sources: Vec<String>,
    pub start_time: i64,
    pub end_time: i64,
    pub resolved: bool,
    pub cancelled: bool,
    /// When primary (bonding-curve) buying ends. `None` for an account layout we don't know.
    pub primary_phase_end: Option<i64>,
    pub last_yes_price: Option<Decimal>,
    pub total_volume_usdc: Decimal,
}

/// Where a market stands, from the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainPhase {
    /// Buyable through Panta's API.
    Primary,
    /// Still trading, but only on Panta's on-chain order book - not through its API.
    Secondary,
}

impl ChainPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            ChainPhase::Primary => "primary",
            ChainPhase::Secondary => "secondary",
        }
    }
}

impl ChainEvent {
    /// Still taking bets: not resolved, not cancelled, and betting hasn't closed.
    pub fn is_open(&self, now: i64) -> bool {
        !self.resolved && !self.cancelled && self.end_time > now
    }

    /// `None` when the layout is unknown - the API's phase is used instead.
    pub fn phase(&self, now: i64) -> Option<ChainPhase> {
        let end = self.primary_phase_end?;
        Some(if now < end { ChainPhase::Primary } else { ChainPhase::Secondary })
    }
}

/// Decodes an `Event` account. `None` for anything else, or data too short to hold the header.
pub fn decode_event(market_id: &str, data: &[u8]) -> Option<ChainEvent> {
    if data.get(..8)? != EVENT_DISCRIMINATOR {
        return None;
    }
    let i64_at = |offset: usize| -> Option<i64> { Some(i64::from_le_bytes(data.get(offset..offset + 8)?.try_into().ok()?)) };
    let u32_at = |offset: usize| -> Option<usize> {
        Some(u32::from_le_bytes(data.get(offset..offset + 4)?.try_into().ok()?) as usize)
    };
    let string_at = |offset: usize| -> Option<(String, usize)> {
        let len = u32_at(offset)?;
        let bytes = data.get(offset + 4..offset + 4 + len)?;
        Some((String::from_utf8_lossy(bytes).trim().to_string(), offset + 4 + len))
    };

    let (question, next) = string_at(TEXT)?;
    let (rules, next) = string_at(next)?;
    let count = u32_at(next)?;
    let mut sources = Vec::new();
    let mut tail = next + 4;
    // A real market names one or two sources; a huge count means the layout isn't what we think.
    if count > 16 {
        return None;
    }
    for _ in 0..count {
        let (source, next) = string_at(tail)?;
        sources.push(source);
        tail = next;
    }

    let last_yes = i64_at(LAST_YES_PRICE)?;
    Some(ChainEvent {
        market_id: market_id.to_string(),
        question,
        rules,
        sources,
        start_time: i64_at(START_TIME)?,
        end_time: i64_at(END_TIME)?,
        resolved: i64_at(RESOLVED_AT)? != 0,
        cancelled: i64_at(CANCELLED_AT)? != 0,
        primary_phase_end: primary_phase_end_offset(data.len()).and_then(|offset| i64_at(tail + offset)),
        last_yes_price: (last_yes > 0).then(|| Decimal::new(last_yes, 9).normalize()),
        total_volume_usdc: Decimal::new(i64_at(TOTAL_VOLUME)?, 6).normalize(),
    })
}

#[derive(Deserialize)]
struct RpcResponse {
    result: Option<Vec<ProgramAccount>>,
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ProgramAccount {
    pubkey: String,
    account: AccountData,
}

#[derive(Deserialize)]
struct AccountData {
    /// `[base64, "base64"]`.
    data: (String, String),
}

/// Every `Event` account the program owns, decoded. Accounts that don't decode are skipped with a
/// warning rather than failing the whole read.
pub async fn list_events(client: &reqwest::Client, rpc_url: &str, program_id: &str) -> Result<Vec<ChainEvent>, AdapterError> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getProgramAccounts",
        "params": [program_id, {
            "encoding": "base64",
            "filters": [{ "memcmp": { "offset": 0, "bytes": BASE64.encode(EVENT_DISCRIMINATOR), "encoding": "base64" } }],
        }],
    });
    let response = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| AdapterError::Venue(format!("panta rpc: {e}")))?;
    let response: RpcResponse = response.json().await.map_err(|e| AdapterError::Parse(format!("panta rpc: {e}")))?;
    if let Some(error) = response.error {
        return Err(AdapterError::Venue(format!("panta rpc: {error}")));
    }
    let accounts = response.result.ok_or_else(|| AdapterError::Parse("panta rpc: no result".into()))?;

    let mut events = Vec::with_capacity(accounts.len());
    for account in accounts {
        let decoded = BASE64.decode(&account.account.data.0).ok().and_then(|data| decode_event(&account.pubkey, &data));
        match decoded {
            Some(event) => events.push(event),
            None => tracing::warn!("panta: could not decode event account {}", account.pubkey),
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from mainnet on 2026-09-24: `fixtures/panta/chain/<market id>.b64`.
    fn fixture(market_id: &str) -> ChainEvent {
        let path = format!("{}/fixtures/panta/chain/{market_id}.b64", env!("CARGO_MANIFEST_DIR"));
        let data = BASE64.decode(std::fs::read_to_string(path).unwrap().trim()).unwrap();
        decode_event(market_id, &data).expect("fixture decodes")
    }

    // 2026-09-24 12:00 UTC, when the fixtures were captured.
    const CAPTURED: i64 = 1_790_251_200;

    const OBI: &str = "DmgQ7eJbVNqAX36NMZxCNbaPmNAKNtQB4o6HG43JQcW1";

    #[test]
    fn decodes_an_open_secondary_market_in_the_older_layout() {
        let event = fixture(OBI);
        assert!(event.question.starts_with("Will Peter Obi be Elected President of Nigeria"), "{}", event.question);
        assert!(event.rules.starts_with("Resolve YES"), "{}", event.rules);
        assert!(event.is_open(CAPTURED));
        assert_eq!(event.phase(CAPTURED), Some(ChainPhase::Secondary));
        assert!(event.total_volume_usdc > Decimal::from(9_000), "{}", event.total_volume_usdc);
        let price = event.last_yes_price.unwrap();
        assert!(price > Decimal::ZERO && price < Decimal::ONE, "{price}");
    }

    #[test]
    fn decodes_an_open_market_in_the_newer_layout() {
        let event = fixture("GXh9iztJTm5v6qDWnR4YcKHbSc3AUZ2VEMGfKEegd92V");
        assert_eq!(event.question, "Will GTA 6 release on November 19th, 2026");
        assert!(event.is_open(CAPTURED));
        assert_eq!(event.phase(CAPTURED), Some(ChainPhase::Secondary));
        assert!(!event.sources.is_empty());
    }

    #[test]
    fn decodes_a_resolved_market() {
        let event = fixture("TXZi2zjbQ3kq14kGREHFns4GFv2TxekTPxgUVBVG9zd");
        assert!(event.question.starts_with("Will Hajji Muhis land at least 10 punches"), "{}", event.question);
        assert!(event.resolved);
        assert!(!event.is_open(CAPTURED));
    }

    #[test]
    fn a_primary_market_is_primary_until_its_phase_ends() {
        let mut event = fixture(OBI);
        event.primary_phase_end = Some(CAPTURED + 3600);
        assert_eq!(event.phase(CAPTURED), Some(ChainPhase::Primary));
        event.primary_phase_end = None;
        assert_eq!(event.phase(CAPTURED), None, "unknown layout defers to the API");
    }

    #[test]
    fn other_accounts_and_short_data_are_not_events() {
        assert!(decode_event("x", &[0; 400]).is_none());
        assert!(decode_event("x", &EVENT_DISCRIMINATOR).is_none());
    }
}
