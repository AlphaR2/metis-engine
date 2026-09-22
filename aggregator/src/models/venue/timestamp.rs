//! Shared timestamp deserialization for venue wire formats.
//!
//! Both integrated venues document their time fields as Unix-second integers and both, in
//! practice, sometimes send ISO-8601 strings for the very same field:
//!
//! - Panta returns integers from the live API but ISO-8601 from a `pk_test_` sandbox.
//! - Jupiter returns integers on most markets but ISO-8601 on some (confirmed live - a `crypto`
//!   page failed to parse with `invalid type: string "2026-02-05T22:30:49.000Z", expected i64`,
//!   which silently cost us that entire category until it was handled).
//!
//! So this accepts either shape rather than trusting either venue's documentation.

use chrono::DateTime;
use serde::{Deserialize, Deserializer};

/// Either shape a venue timestamp field has been observed to take.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawTimestamp {
    Unix(i64),
    Iso8601(String),
}

impl RawTimestamp {
    /// Unix seconds, or `None` if a string couldn't be parsed.
    fn to_unix(&self) -> Option<i64> {
        match self {
            RawTimestamp::Unix(secs) => Some(*secs),
            RawTimestamp::Iso8601(raw) => DateTime::parse_from_rfc3339(raw).map(|dt| dt.timestamp()).ok(),
        }
    }
}

/// Accepts a Unix-second integer or an ISO-8601 string, normalizing to Unix seconds.
///
/// Falls back to 0 on an unparseable string rather than failing the whole row - a garbled
/// timestamp on one market shouldn't cost us every other market on the same page. `0` is treated
/// as "unknown" by `VenueMarket::is_bettable`, not as 1970.
pub fn deserialize_timestamp<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(RawTimestamp::deserialize(deserializer)?.to_unix().unwrap_or(0))
}

/// As `deserialize_timestamp`, for a field that may be absent or explicitly null.
pub fn deserialize_optional_timestamp<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<RawTimestamp>::deserialize(deserializer)?.and_then(|raw| raw.to_unix()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Required(#[serde(deserialize_with = "deserialize_timestamp")] i64);

    #[derive(Deserialize)]
    struct Optional(#[serde(default, deserialize_with = "deserialize_optional_timestamp")] Option<i64>);

    fn required(json: &str) -> i64 {
        serde_json::from_str::<Required>(json).unwrap().0
    }

    fn optional(json: &str) -> Option<i64> {
        serde_json::from_str::<Optional>(json).unwrap().0
    }

    #[test]
    fn accepts_a_unix_integer() {
        assert_eq!(required("1835514000"), 1_835_514_000);
    }

    #[test]
    fn accepts_an_iso8601_string() {
        assert_eq!(required("\"2026-01-01T00:00:00Z\""), 1_767_225_600);
    }

    #[test]
    fn accepts_the_millisecond_precision_form_jupiter_sends() {
        // The exact shape that broke a whole Jupiter category before this was handled.
        assert_eq!(required("\"2026-02-05T22:30:49.000Z\""), 1_770_330_649);
    }

    #[test]
    fn falls_back_to_zero_on_an_unparseable_string() {
        assert_eq!(required("\"not-a-timestamp\""), 0);
    }

    #[test]
    fn optional_handles_null_and_both_shapes() {
        assert_eq!(optional("null"), None);
        assert_eq!(optional("1835514000"), Some(1_835_514_000));
        assert_eq!(optional("\"2026-01-01T00:00:00Z\""), Some(1_767_225_600));
        assert_eq!(optional("\"garbled\""), None);
    }
}
