//! Routes for actions that need a signature.
//!
//! Each of these builds an unsigned transaction and hands it back. **None of them signs, and none
//! of them broadcasts** — the caller's wallet does both, and the caller tells us the signature
//! afterwards via `confirm`. Metis never holds a key.
//!
//! Every route here is behind a config flag that is **off by default**. A buy spends real USDC and
//! a created market is a permanent public artifact carrying the creator's wallet; neither should
//! become possible because something was deployed.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::errors::ApiError;
use crate::models::execution::{
    BuyIntent, ClaimIntent, ConfirmOutcome, CreateMarketIntent, UnsignedTransaction,
};
use crate::state::AppState;
use crate::utils::time::now_unix;

/// What a caller gets back for anything that needs signing.
#[derive(Debug, Serialize, ToSchema)]
pub struct BuildResponse {
    #[serde(flatten)]
    pub transaction: UnsignedTransaction,
    /// Seconds until the venue's window closes, computed at response time.
    ///
    /// Sent alongside the absolute `expires_at` because these windows are short — Panta's blockhash
    /// is about 60 seconds — and a client with a skewed clock would otherwise mis-state the one
    /// number that decides whether signing is still worth attempting.
    pub expires_in_secs: i64,
}

/// Tells us a built transaction was broadcast.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ConfirmRequest {
    pub venue_id: String,
    /// The `venue_ref` from the build response.
    pub venue_ref: String,
    /// Base58 transaction signature.
    pub signature: String,
}

/// Looks up an adapter, or says plainly that the venue isn't wired up.
fn adapter(
    state: &AppState,
    venue_id: &str,
) -> Result<std::sync::Arc<dyn crate::services::VenueAdapter>, ApiError> {
    state.aggregator.venue(venue_id)
        .ok_or_else(|| {
            ApiError::BadRequest(format!(
                "no venue '{venue_id}' is enabled — check ENABLED_VENUES"
            ))
        })
}

/// The refusal a disabled flag produces. Says which switch, so an operator isn't left guessing.
fn disabled(what: &str, flag: &str) -> ApiError {
    ApiError::BadRequest(format!(
        "{what} is disabled on this deployment. Set {flag}=true to enable it. \
         It is off by default because it moves real funds and cannot be undone."
    ))
}

fn build_response(transaction: UnsignedTransaction) -> Json<BuildResponse> {
    let expires_in_secs = transaction.seconds_remaining(now_unix());
    Json(BuildResponse { transaction, expires_in_secs })
}

#[utoipa::path(
    post,
    path = "/execution/orders",
    tag = "execution",
    request_body = BuyIntent,
    responses(
        (status = 200, body = BuildResponse, description = "An unsigned buy transaction for the caller's wallet to sign."),
        (status = 400, description = "Execution disabled, unknown venue, or the venue rejected the parameters"),
    )
)]
/// Builds a buy. Does not sign and does not broadcast.
pub async fn build_order(
    State(state): State<AppState>,
    Json(intent): Json<BuyIntent>,
) -> Result<Json<BuildResponse>, ApiError> {
    if !state.execution_enabled {
        return Err(disabled("Trading", "EXECUTION_ENABLED"));
    }
    let venue = adapter(&state, &intent.venue_id)?;
    let transaction = venue.build_buy(&intent).await?;
    Ok(build_response(transaction))
}

#[utoipa::path(
    post,
    path = "/execution/markets",
    tag = "execution",
    request_body = CreateMarketIntent,
    responses(
        (status = 200, body = BuildResponse, description = "An unsigned market-creation transaction."),
        (status = 400, description = "Creation disabled, unknown venue, or invalid market parameters"),
    )
)]
/// Builds a new market. The signing wallet pays the fee and earns any creator fees it accrues.
pub async fn build_market(
    State(state): State<AppState>,
    Json(intent): Json<CreateMarketIntent>,
) -> Result<Json<BuildResponse>, ApiError> {
    if !state.market_creation_enabled {
        return Err(disabled("Market creation", "MARKET_CREATION_ENABLED"));
    }
    let venue = adapter(&state, &intent.venue_id)?;
    let transaction = venue.build_create_market(&intent).await?;
    Ok(build_response(transaction))
}

#[utoipa::path(
    post,
    path = "/execution/claims",
    tag = "execution",
    request_body = ClaimIntent,
    responses(
        (status = 200, body = BuildResponse, description = "An unsigned claim transaction."),
        (status = 400, description = "Execution disabled, or the venue has no claim flow"),
    )
)]
/// Builds a claim for winnings or accrued creator fees.
pub async fn build_claim(
    State(state): State<AppState>,
    Json(intent): Json<ClaimIntent>,
) -> Result<Json<BuildResponse>, ApiError> {
    // A claim only ever pays in, but it rides the same venue plumbing as everything else here, so
    // it stays behind the same switch rather than being quietly always-on.
    if !state.execution_enabled {
        return Err(disabled("Claiming", "EXECUTION_ENABLED"));
    }
    let venue = adapter(&state, &intent.venue_id)?;
    let transaction = venue.build_claim(&intent).await?;
    Ok(build_response(transaction))
}

#[utoipa::path(
    post,
    path = "/execution/confirm",
    tag = "execution",
    request_body = ConfirmRequest,
    responses(
        (status = 200, body = ConfirmOutcome, description = "What the venue made of the broadcast transaction."),
        (status = 400, description = "Unknown venue, or a reference the venue doesn't recognise"),
    )
)]
/// Tells the venue a transaction was broadcast, and reports back what became of it.
///
/// Safe to call more than once with the same reference and signature — Panta's register is
/// explicitly idempotent on that pair, so a caller who loses the response can retry rather than be
/// left unsure whether their money bought anything.
pub async fn confirm(
    State(state): State<AppState>,
    Json(req): Json<ConfirmRequest>,
) -> Result<Json<ConfirmOutcome>, ApiError> {
    if req.signature.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "signature is required - to check an action's status without one, GET /execution/{venue_id}/{venue_ref}".to_string(),
        ));
    }
    let venue = adapter(&state, &req.venue_id)?;
    let outcome = venue.confirm(&req.venue_ref, &req.signature).await?;
    Ok(Json(outcome))
}

/// What each venue can actually do, so a UI asks instead of assuming.
#[derive(Debug, Serialize, ToSchema)]
pub struct VenueAbilities {
    pub venue_id: String,
    pub can_execute: bool,
    pub can_create_markets: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AbilitiesResponse {
    /// False when `EXECUTION_ENABLED` is off, whatever the venues support.
    pub execution_enabled: bool,
    /// False when `MARKET_CREATION_ENABLED` is off.
    pub market_creation_enabled: bool,
    pub venues: Vec<VenueAbilities>,
}

#[utoipa::path(
    get,
    path = "/execution/abilities",
    tag = "execution",
    responses((status = 200, body = AbilitiesResponse, description = "Which actions are possible, per venue and overall."))
)]
/// Reports what is possible here — both the deployment's switches and each venue's own support.
///
/// A UI needs both: a venue can support creation while this deployment has it switched off, and
/// showing a create button in that case would only produce a refusal.
pub async fn abilities(State(state): State<AppState>) -> Json<AbilitiesResponse> {
    let venues = state
        .aggregator
        .venues
        .iter()
        .map(|venue| {
            let caps = venue.capabilities();
            VenueAbilities {
                venue_id: venue.id().to_string(),
                can_execute: caps.can_execute,
                can_create_markets: caps.can_create_markets,
            }
        })
        .collect();

    Json(AbilitiesResponse {
        execution_enabled: state.execution_enabled,
        market_creation_enabled: state.market_creation_enabled,
        venues,
    })
}

#[utoipa::path(
    get,
    path = "/execution/{venue_id}/{venue_ref}",
    tag = "execution",
    params(
        ("venue_id" = String, Path, description = "Venue the action was built on"),
        ("venue_ref" = String, Path, description = "The venue_ref from the build response"),
    ),
    responses(
        (status = 200, body = ConfirmOutcome, description = "Current status of a submitted action."),
        (status = 400, description = "Unknown venue, or a venue with no read-only status lookup"),
    )
)]
/// Polls a submitted action's status. Read-only - never registers or re-sends anything, so a
/// venue without a status lookup (Panta today) says so instead of reporting a false failure.
pub async fn status(
    State(state): State<AppState>,
    Path((venue_id, venue_ref)): Path<(String, String)>,
) -> Result<Json<ConfirmOutcome>, ApiError> {
    let venue = adapter(&state, &venue_id)?;
    let outcome = venue.status(&venue_ref).await?;
    Ok(Json(outcome))
}
