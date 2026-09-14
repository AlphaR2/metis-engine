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
}

impl From<MetisError> for ProgramError {
    fn from(e: MetisError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
