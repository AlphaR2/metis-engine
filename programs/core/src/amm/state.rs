use bytemuck::{Pod, Zeroable};
use pinocchio::{Address, error::ProgramError};

use crate::constants::MAX_OUTCOMES;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketStatus {
    Uninitialized = 0,
    Open = 1,
    Closed = 2,
    Settled = 3,
    Cancelled = 4,
}

impl MarketStatus {
    /// Unlike oracle/'s CFootballMatchPhase::can_transition_to, this is NOT idempotent-retry-
    /// friendly on purpose: re-settling an already-Settled market must never silently succeed a
    /// second time (unlike an oracle score re-send, a repeat settlement is a correctness risk,
    /// not a harmless retry). Only one transition exists today; docs/plan.md item 6 will add
    /// Open -> Cancelled here when that work starts.
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!((self, next), (Self::Open, Self::Settled))
    }
}

impl TryFrom<u8> for MarketStatus {
    type Error = ProgramError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(MarketStatus::Uninitialized),
            1 => Ok(MarketStatus::Open),
            2 => Ok(MarketStatus::Closed),
            3 => Ok(MarketStatus::Settled),
            4 => Ok(MarketStatus::Cancelled),
            _ => Err(ProgramError::InvalidAccountData),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Market {
    /// Recorded shares for each side. For a simple binary market we have [xa, xb].
    pub outcome_shares: [u64; MAX_OUTCOMES],
    /// The virtual liquidity param for each market. Frozen at initialize_market, k × symmetric_seed(fee, outcomes)
     pub virtual_per_side: u64,
    /// Total money paid into the vault. Mirror of the vault's balance
    pub total_collected: u64,
    /// Unix timestamp (seconds) after which place_bet stops accepting bets.
    pub bet_close_ts: i64,
    /// Timestamp for primary trade/ amm end. This is a signal to reject any buy into market and move all trade to secondary.
    pub primary_end: i64,
    /// The creator-chosen nonce this market's PDA was derived from.
    pub nonce: u64,
    /// Creator of the market. Permissionless, anyone can create a market
    pub creator: Address,
    /// The oracle/ account this market settles against
    pub oracle: Address,
    /// The operational key assigned to run cranks/actions for this specific market
    pub delegate: Address,
    /// This market's USDC vault which is an SPL token account, authority = market PDA.
    pub vault: Address,
    /// The stablecoin this market is denominated in.
    pub quote_mint: Address,
    pub discriminator: [u8; 8],
    /// How many of `outcome_shares` are actually in play for this market.
    /// Always <= MAX_OUTCOMES.
    pub num_outcomes: u8,
    /// MarketStatus as u8.
    pub status: u8,
    /// Index into outcome_shares, or OUTCOME_UNRESOLVED.
    pub winning_outcome: u8,
    pub bump: u8,
    pub _padding: [u8; 4],
}

const _: () = assert!(core::mem::size_of::<Market>() == 232);
const _: () = assert!(core::mem::align_of::<Market>() == 8);

// ── Discriminators owned by this part
// Do not hand-edit. Regenerate with `cargo run --bin compute-discriminators`
// if a struct is renamed or a new account/instruction is added.
pub const MARKET_DISCRIMINATOR: [u8; 8] = [219, 190, 213, 55, 0, 227, 198, 154];
pub const INITIALIZE_MARKET: [u8; 8] = [35, 35, 189, 193, 155, 48, 170, 203];
pub const PLACE_BET: [u8; 8] = [222, 62, 67, 220, 63, 166, 126, 33];

// Tests for the market's on-chain layout. To see the printed output:
//   cargo test -p metis-core --lib amm::state -- --nocapture
#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::{bytes_of, from_bytes};

    // Market is read straight out of account data with no deserialisation
    // step, so its byte layout IS its storage format. This proves a written
    // struct reads back identical - if a field were added without adjusting
    // the padding, this is what would catch it.
    #[test]
    fn market_round_trips_through_zero_copy() {
        let market = Market {
            virtual_per_side: 424,
            outcome_shares: [7, 9],
            total_collected: 16,
            bet_close_ts: 1_800_000_000,
            primary_end: 1_800_000_000 - crate::constants::PRIMARY_WINDOW_SECS,
            creator: Default::default(),
            oracle: Default::default(),
            delegate: Default::default(),
            nonce: 42,
            quote_mint: Default::default(),
            vault: Default::default(),
            discriminator: MARKET_DISCRIMINATOR,
            num_outcomes: 2,
            status: MarketStatus::Open as u8,
            winning_outcome: crate::constants::OUTCOME_UNRESOLVED,
            bump: 254,
            _padding: [0; 4],
        };
        let bytes = bytes_of(&market);
        let recovered: &Market = from_bytes(bytes);

        println!(
            "Market is {} bytes on chain, align {}",
            core::mem::size_of::<Market>(),
            core::mem::align_of::<Market>()
        );
        println!("  outcome_shares:  {:?}", recovered.outcome_shares);
        println!("  virtual_per_side:{}", recovered.virtual_per_side);
        println!("  total_collected: {}", recovered.total_collected);
        println!("  bet_close_ts:    {} (kickoff)", recovered.bet_close_ts);
        println!(
            "  primary_end:     {} (betting shuts here)",
            recovered.primary_end
        );
        println!("  nonce:           {}", recovered.nonce);
        println!("  num_outcomes:    {}", recovered.num_outcomes);
        println!("  status:          {}", recovered.status);

        assert_eq!(*recovered, market);
    }

    // status is stored as a plain u8, so the enum and the number are only
    // connected by these casts. Nothing keeps them in step automatically -
    // this test is what catches a renumbering, and it has done so before.
    #[test]
    fn market_status_try_from_round_trips() {
        for (n, expected) in [
            (0u8, MarketStatus::Uninitialized),
            (1, MarketStatus::Open),
            (2, MarketStatus::Closed),
            (3, MarketStatus::Settled),
            (4, MarketStatus::Cancelled),
        ] {
            let got = MarketStatus::try_from(n).unwrap();
            println!("status byte {n} -> {got:?}");
            assert_eq!(got, expected);
        }

        // Anything outside the known set is rejected rather than silently
        // treated as some valid state.
        println!("status byte 5 -> rejected (as it should be)");
        assert!(MarketStatus::try_from(5).is_err());
    }

    // Only Open -> Settled is a legal transition today. Checked exhaustively rather than
    // spot-checked, since this is the guard settle_market's whole solvency story rests on -
    // including the case that matters most: Settled -> Settled (re-settling) must stay rejected,
    // unlike oracle/'s idempotent-retry-friendly equivalent.
    #[test]
    fn market_status_can_transition_to_is_exhaustive() {
        let all = [
            MarketStatus::Uninitialized,
            MarketStatus::Open,
            MarketStatus::Closed,
            MarketStatus::Settled,
            MarketStatus::Cancelled,
        ];

        for from in all {
            for to in all {
                let allowed = from.can_transition_to(to);
                println!("{from:?} -> {to:?}: {allowed}");
                assert_eq!(
                    allowed,
                    (from, to) == (MarketStatus::Open, MarketStatus::Settled),
                    "unexpected transition legality for {from:?} -> {to:?}"
                );
            }
        }
    }
}
