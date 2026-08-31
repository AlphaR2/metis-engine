use pinocchio::error::ProgramError;

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenStakeError {
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
}

impl From<OpenStakeError> for ProgramError {
    fn from(e: OpenStakeError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
