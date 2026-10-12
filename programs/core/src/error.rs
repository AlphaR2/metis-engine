use pinocchio::error::ProgramError;

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetisError {
    /// Account's discriminator didn't match the expected type for this
    /// operation — e.g. a Vault passed where a Position was expected.
    DiscriminatorMismatch = 0,

    /// place_bet called on a market that isn't MarketStatus::Open.
    MarketNotOpen = 1,

    /// claim called before the market is MarketStatus::Settled.
    MarketNotSettled = 2,

    /// settle_market called on a market that's already Settled, or any
    /// other MarketStatus::valid_transition rejection.
    InvalidStatusTransition = 3,

    /// Outcome index >= num_outcomes, or >= MAX_OUTCOMES.
    InvalidOutcome = 4,

    /// claim called twice on the same Position.
    AlreadyClaimed = 5,

    /// The vault holds LESS than rent_minimum + total_collected — an
    /// actual shortfall, not just drift. A solvency floor, not an exact
    /// match: unsolicited lamports landing in the vault (anyone can push
    /// lamports into any account) must never brick settlement, so this
    /// only fires when there's genuinely not enough to cover payouts.
    VaultBalanceMismatch = 6,

    /// place_bet payment too small to mint a non-zero share delta.
    PaymentTooSmall = 7,

    /// place_bet called after the market's bet_close_ts has passed.
    MarketClosed = 8,

    /// place_bet's actual share delta came in under the caller's
    /// min_shares_out — the curve moved against them before the tx landed.
    SlippageExceeded = 9,

    /// place_bet's deposit exceeds Config.max_in_bps_of_vault's share of the
    /// market's priced depth (docs/mvp.md 1.2/1.6's sizing cap).
    TradeCapExceeded = 10,

    /// place_bet's deposit, once executed, would move probability by more
    /// than MAX_PRICE_IMPACT_BPS — docs/mvp.md 1.2's forward-only hard
    /// impact ceiling. Never an inversion: this checks the trade that
    /// already happened, it never solves for a deposit size.
    PriceImpactExceeded = 11,

    /// A bettor-role account (place_bet's or claim's `bettor`) didn't sign.
    BettorMustSign = 12,

    /// initialize_market's `creator` didn't sign.
    CreatorMustSign = 13,

    /// A protocol-authority-role account didn't sign: settle_market's
    /// `authority`, initialize_config's `authority`, or update_config's
    /// `admin` — all the same "the one key allowed to do this" role.
    AuthorityMustSign = 14,

    /// An account required to be writable wasn't. Purely structural — which
    /// specific account failed is visible from the instruction's own account
    /// list, so this is reused across every require_writable call site
    /// rather than growing one variant per account.
    AccountNotWritable = 15,

    /// The `market` account failed its initialized-and-owned-by-this-program
    /// check (place_bet, claim, settle_market).
    InvalidMarketAccount = 16,

    /// The `config` account failed its initialized-and-owned-by-this-program
    /// check (place_bet, initialize_market, update_config).
    InvalidConfigAccount = 17,

    /// A derived PDA (config/market/vault/outcome-mint) didn't match the
    /// account actually passed in.
    InvalidPda = 18,

    /// An account didn't match a value stored on another account it's
    /// supposed to correspond to — e.g. the vault passed in isn't the one
    /// Market.vault recorded, or an ATA's mint/owner doesn't match what
    /// claim expects. Distinct from InvalidPda: this is a stored-reference
    /// check, not a fresh derivation.
    WrongAccountReference = 19,

    /// Not one of Config.root's own signers, where root approval was required
    /// (attest_authority/remove_authority).
    UnauthorizedAuthority = 20,

    /// The root-bearing account already exists / isn't empty at initialize time.
    AlreadyInitialized = 21,

    /// A RootBearing (Config) or DelegatedAuthority account's discriminator, stored address
    /// field, or general shape didn't check out.
    InvalidAuthorityAccount = 22,

    /// `Rent::try_minimum_balance` failed while creating a DelegatedAuthority account.
    RentCalculationFailed = 23,

    /// The same attester/voter tried to attest/vote twice.
    DuplicateAuthority = 24,

    /// Tried to attest/vote after the quorum was already complete.
    AttestationComplete = 25,

    /// A non-root attester tried to finish off a candidate no root has touched yet.
    MissingRootAttestation = 26,

    /// A Config dial is outside the hardcoded bound in constants.rs.
    ConfigValueOutOfBounds = 27,

    /// The instruction needs two distinct root signers and didn't get them.
    TwoRootSignersRequired = 28,

    /// The program is paused by a root signer; nothing that moves money or changes trust runs.
    ProgramPaused = 29,

    /// Unpause needs the guardian plus the required number of distinct root signers.
    UnpauseSignaturesMissing = 30,

    // ── Oracle-driven settlement
    /// The oracle account passed isn't the one this market was created against.
    OracleMismatch = 31,
    /// The oracle isn't a real football oracle (wrong owner, size or discriminator).
    InvalidOracleAccount = 32,
    /// The oracle has no final result yet.
    OracleNotFinal = 33,
    /// The oracle's result is final but the market's dispute window hasn't passed.
    DisputeWindowOpen = 34,
    /// The oracle voided the result; the market must be cancelled, not settled.
    OracleVoided = 35,
    /// The oracle is already past kickoff, finished or voided, or kickoff is too close.
    OracleNotScheduled = 36,
    /// The market's condition isn't one the program knows.
    InvalidCondition = 37,

    // ── Cancel, refund, sweep, close
    /// Cancelling needs a voided oracle, a result overdue by CANCEL_AFTER_SECS, or two roots.
    CancelNotAllowed = 38,
    MarketNotCancelled = 39,
    /// Nothing is owed to the sweeping party (already swept, or never settled).
    NothingToSweep = 40,
    /// close_market before CLAIM_WINDOW_SECS has passed since settlement/cancellation.
    ClaimWindowOpen = 41,
    /// close_primary before primary_end.
    PrimaryStillOpen = 42,
    /// Metadata text is empty, too long, not UTF-8, or has control characters.
    InvalidMetadata = 43,
    /// close_market while the creator's fees are still unclaimed. (The protocol's share doesn't
    /// block closing: it is swept to the treasury as part of it.)
    FeesUnswept = 44,

    // ── Gated instructions
    /// The signer is neither a root nor an attested operator.
    OperatorRequired = 45,
    /// forfeit_creator_fees before CREATOR_CLAIM_SECS has passed since settlement.
    ForfeitTooEarly = 46,
    /// forfeit_creator_fees on a market whose creator has nothing unclaimed.
    NothingToForfeit = 47,

    // ── DBC backing
    /// A backed market was requested while Config.dbc_backing_enabled is off.
    BackingDisabled = 48,
    /// The raise deadline is outside [MIN_RAISE_SECS, MAX_RAISE_SECS] from now, or after betting would close.
    RaiseDeadlineInvalid = 49,
    /// activate_market after the raise deadline.
    RaiseDeadlinePassed = 50,
    /// cancel_pending_market before the deadline, by someone who is not an operator.
    RaiseNotExpired = 51,
    /// The backer mint is not the one the market recorded, still has a mint or freeze authority, or has no supply.
    BackerMintInvalid = 52,
    /// The vault does not hold the market's seed yet.
    SeedNotFunded = 53,
    /// claim_backer on a market that was not seeded by backers.
    NotBackerMarket = 54,
    /// claim_backer with no backer tokens, or on a market whose seed shares are already gone.
    NothingToRedeem = 55,
}

impl From<MetisError> for ProgramError {
    fn from(e: MetisError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

impl metis_authority::errors::AuthorityErrors for MetisError {
    fn must_sign() -> Self {
        MetisError::AuthorityMustSign
    }
    fn unauthorized_authority() -> Self {
        MetisError::UnauthorizedAuthority
    }
    fn already_initialized() -> Self {
        MetisError::AlreadyInitialized
    }
    fn invalid_authority_pda() -> Self {
        MetisError::InvalidPda
    }
    fn invalid_authority() -> Self {
        MetisError::InvalidAuthorityAccount
    }
    fn rent_calculation_failed() -> Self {
        MetisError::RentCalculationFailed
    }
    fn duplicate_authority() -> Self {
        MetisError::DuplicateAuthority
    }
    fn attestation_complete() -> Self {
        MetisError::AttestationComplete
    }
    fn missing_root_attestation() -> Self {
        MetisError::MissingRootAttestation
    }
    fn paused() -> Self {
        MetisError::ProgramPaused
    }
    fn unpause_signatures_missing() -> Self {
        MetisError::UnpauseSignaturesMissing
    }
}
