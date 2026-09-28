//! Raw venue inspection.
//!
//! Returns a venue's response **exactly as it sent it** — nothing parsed, nothing filtered, nothing
//! normalised. Every other route in this service has already dropped dead rows, applied fallbacks
//! and reshaped fields before you see anything, which is exactly what you don't want when the
//! question is "what is this venue actually giving us".
//!
//! That question has come up repeatedly with Panta: markets with no title, a cursor that doesn't
//! advance, filter params that are silently ignored. Being able to see the unprocessed payload in
//! the UI is what separates *their* bug from *ours*.
//!
//! GET only, operator-only (behind `ADMIN_TOKEN`), and limited to each venue's read-only catalog
//! paths. This borrows the venue's credentials: a relay that could also POST would be a way to
//! place orders through someone else's key, and an open GET relay let anyone spend our rate limit
//! or reach other endpoints on the venue's host (audit A3).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::collections::HashMap;
use utoipa::ToSchema;

use crate::errors::ApiError;
use crate::state::AppState;

/// A summary of which fields the venue actually populated.
///
/// The raw body answers "what did they send"; this answers "and how much of it is real". With a
/// venue that returns a full-looking object where most fields are empty strings, a coverage count
/// is the difference between a payload that looks fine and one that is.
#[derive(Debug, Serialize, ToSchema)]
pub struct FieldCoverage {
    pub field: String,
    /// Rows where this field is present and not empty.
    pub populated: usize,
    pub total: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RawResponse {
    pub venue_id: String,
    pub path: String,
    /// The venue's body, parsed only far enough to embed it as JSON. If it wasn't JSON at all, it
    /// arrives as a string so you can still read it.
    pub body: serde_json::Value,
    /// Per-field population counts, when the body held an array of objects we could find.
    pub coverage: Vec<FieldCoverage>,
    /// How many rows the coverage was computed over.
    pub rows: usize,
}

/// Finds the first array of objects in a response, wherever the venue chose to put it.
///
/// Envelopes differ - Panta uses `items`, Jupiter `data`, Pascal `markets` - so rather than
/// hardcode each, this looks for the first array of objects at the top level or one level down.
fn find_rows(body: &serde_json::Value) -> Vec<&serde_json::Map<String, serde_json::Value>> {
    fn as_rows(v: &serde_json::Value) -> Option<Vec<&serde_json::Map<String, serde_json::Value>>> {
        let arr = v.as_array()?;
        let rows: Vec<_> = arr.iter().filter_map(|i| i.as_object()).collect();
        (!rows.is_empty()).then_some(rows)
    }
    if let Some(rows) = as_rows(body) {
        return rows;
    }
    body.as_object()
        .into_iter()
        .flat_map(|o| o.values())
        .find_map(as_rows)
        .unwrap_or_default()
}

/// Counts how many rows actually carry a usable value for each field.
///
/// "Usable" excludes null, the empty string and the empty array - a field present but blank is the
/// precise failure mode being looked for here, and counting it as populated would hide it.
fn coverage_of(rows: &[&serde_json::Map<String, serde_json::Value>]) -> Vec<FieldCoverage> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut seen: Vec<&str> = Vec::new();

    for row in rows {
        for (key, value) in row.iter() {
            if !seen.contains(&key.as_str()) {
                seen.push(key.as_str());
            }
            let populated = match value {
                serde_json::Value::Null => false,
                serde_json::Value::String(s) => !s.trim().is_empty(),
                serde_json::Value::Array(a) => !a.is_empty(),
                _ => true,
            };
            if populated {
                *counts.entry(key.as_str()).or_insert(0) += 1;
            }
        }
    }

    let mut out: Vec<FieldCoverage> = seen
        .into_iter()
        .map(|field| FieldCoverage {
            field: field.to_string(),
            populated: counts.get(field).copied().unwrap_or(0),
            total: rows.len(),
        })
        .collect();
    // Emptiest first: the whole point is to make the gaps obvious.
    out.sort_by_key(|c| (c.populated, c.field.clone()));
    out
}

#[utoipa::path(
    get,
    path = "/venues/{venue_id}/raw/{path}",
    tag = "raw",
    params(
        ("venue_id" = String, Path, description = "Venue to query, e.g. 'panta'"),
        ("path" = String, Path, description = "Path on the venue's own API, e.g. 'markets/'"),
    ),
    responses(
        (status = 200, body = RawResponse, description = "The venue's untouched response, plus field coverage. The HTTP status is the venue's own."),
        (status = 400, description = "Unknown venue, a path outside the venue's allow-list, or a dot segment"),
        (status = 401, description = "Missing or invalid X-Admin-Token."),
    )
)]
/// Proxies a GET to the venue and returns the body untouched.
///
/// Any query string is forwarded as-is, so a venue's own filters can be exercised directly — which
/// is how you find out whether it honours them.
pub async fn raw_get(
    State(state): State<AppState>,
    Path((venue_id, path)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let venue = state.aggregator.venue(&venue_id)
        .ok_or_else(|| ApiError::BadRequest(format!("no venue '{venue_id}' is enabled")))?;

    let first_segment = path.trim_start_matches('/').split('/').next().unwrap_or_default();
    let allowed = venue.raw_path_prefixes();
    if !allowed.contains(&first_segment) {
        return Err(ApiError::BadRequest(format!(
            "raw inspection on {venue_id} is limited to paths under: {}",
            allowed.join(", ")
        )));
    }

    let query: Vec<(String, String)> = params.into_iter().collect();
    let (upstream_status, text) = venue.raw_get(&path, &query).await?;
    let status = StatusCode::from_u16(upstream_status).unwrap_or(StatusCode::BAD_GATEWAY);

    // Embed the body as JSON when it is JSON, and as a string when it isn't - a venue returning
    // HTML or a bare error is itself informative and shouldn't become a parse failure.
    let body: serde_json::Value =
        serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));

    let rows = find_rows(&body);
    let coverage = coverage_of(&rows);

    Ok((
        status,
        Json(RawResponse {
            venue_id,
            path,
            rows: rows.len(),
            coverage,
            body,
        }),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_are_found_inside_whatever_envelope_a_venue_uses() {
        // Panta wraps in `items`, Jupiter in `data`, Pascal in `markets`.
        for key in ["items", "data", "markets"] {
            let body = json!({ key: [{"a": 1}, {"a": 2}] });
            assert_eq!(find_rows(&body).len(), 2, "should find rows under '{key}'");
        }
    }

    #[test]
    fn a_bare_array_is_also_accepted() {
        assert_eq!(find_rows(&json!([{"a": 1}])).len(), 1);
    }

    #[test]
    fn a_body_with_no_rows_yields_nothing_rather_than_failing() {
        assert!(find_rows(&json!({"status": "ok"})).is_empty());
    }

    #[test]
    fn an_empty_string_counts_as_unpopulated() {
        // This is the exact Panta failure: `title` present on every row, blank on every row.
        let body = json!({ "items": [
            {"title": "", "id": "a"},
            {"title": "   ", "id": "b"},
            {"title": "Real", "id": "c"},
        ]});
        let cov = coverage_of(&find_rows(&body));
        let title = cov.iter().find(|c| c.field == "title").unwrap();
        assert_eq!((title.populated, title.total), (1, 3));
        let id = cov.iter().find(|c| c.field == "id").unwrap();
        assert_eq!((id.populated, id.total), (3, 3));
    }

    #[test]
    fn null_and_empty_arrays_count_as_unpopulated() {
        let body = json!({ "items": [{"a": null, "b": [], "c": 0, "d": false}] });
        let cov = coverage_of(&find_rows(&body));
        let get = |f: &str| cov.iter().find(|c| c.field == f).unwrap().populated;
        assert_eq!(get("a"), 0);
        assert_eq!(get("b"), 0);
        // Zero and false are real values, not absences.
        assert_eq!(get("c"), 1);
        assert_eq!(get("d"), 1);
    }

    #[test]
    fn coverage_lists_the_emptiest_fields_first() {
        let body = json!({ "items": [{"full": "x", "empty": ""}, {"full": "y", "empty": ""}] });
        let cov = coverage_of(&find_rows(&body));
        assert_eq!(cov[0].field, "empty", "the gaps are the point, so they lead");
    }
}
