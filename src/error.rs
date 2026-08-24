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

    /// A stored total_collected didn't match the vault's actual lamport
    /// balance — the §19.5 divergence check from the design doc. Should
    /// never fire if Phase 1 accounting is correct; exists as a hard stop
    /// rather than silently trusting a drifted number.
    VaultBalanceMismatch = 6,

    /// place_bet payment too small to mint a non-zero share delta.
    PaymentTooSmall = 7,
}

impl From<OpenStakeError> for ProgramError {
    fn from(e: OpenStakeError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
