use bytemuck::{Pod, Zeroable};
use pinocchio::{error::ProgramError, Address};

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
    /// Total money paid into the vault. Mirror of the vault's balance
    pub total_collected: u64,
    /// Unix timestamp (seconds) after which place_bet stops accepting bets.
    pub bet_close_ts: i64,
    /// Creator of the market. Permissionless, anyone can create a market
    pub creator: Address,
    /// The one fixed protocol authority allowed to call settle_market (see constants::AUTHORITY)
    ///
    /// TODO(single-point-of-failure): one keypair settling every market is not ideal so do this better afterwards
    pub authority: Address,
    /// This market's own vault. One vault per market, never shared
    pub vault: Address,
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

const _: () = assert!(core::mem::size_of::<Market>() == 144);
const _: () = assert!(core::mem::align_of::<Market>() == 8);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]

/// A bettor's position, one PDA per (market, owner) pair.
pub struct Position {
    /// This bettor's shares, one per outcome.
    pub outcome_shares: [u64; MAX_OUTCOMES],
    /// The market this position belongs to.
    pub market: Address,
    pub owner: Address,
    pub discriminator: [u8; 8],
    /// 0 = unclaimed, 1 = claimed.
    pub claimed: u8,
    pub bump: u8,
    pub _padding: [u8; 6],
}

const _: () = assert!(core::mem::size_of::<Position>() == 96);
const _: () = assert!(core::mem::align_of::<Position>() == 8);

// ── Discriminators owned by this part ────────────────────────────────
// Do not hand-edit. Regenerate with `cargo run --bin compute-discriminators`
// if a struct is renamed or a new account/instruction is added.
pub const MARKET_DISCRIMINATOR:   [u8; 8] = [219, 190, 213, 55, 0, 227, 198, 154];
pub const POSITION_DISCRIMINATOR: [u8; 8] = [170, 188, 143, 228, 122, 64, 247, 208];
pub const INITIALIZE_MARKET: [u8; 8] = [35, 35, 189, 193, 155, 48, 170, 203];
pub const PLACE_BET:         [u8; 8] = [222, 62, 67, 220, 63, 166, 126, 33];

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::{bytes_of, from_bytes};

    #[test]
    fn market_round_trips_through_zero_copy() {
        let market = Market {
            outcome_shares: [7, 9],
            total_collected: 16,
            bet_close_ts: 1_800_000_000,
            creator: Default::default(),
            authority: Default::default(),
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
        assert_eq!(*recovered, market);
    }

    #[test]
    fn position_round_trips_through_zero_copy() {
        let position = Position {
            outcome_shares: [3, 4],
            market: Default::default(),
            owner: Default::default(),
            discriminator: POSITION_DISCRIMINATOR,
            claimed: 1,
            bump: 253,
            _padding: [0; 6],
        };
        let bytes = bytes_of(&position);
        let recovered: &Position = from_bytes(bytes);
        assert_eq!(*recovered, position);
    }

    #[test]
    fn market_status_try_from_round_trips() {
        assert_eq!(MarketStatus::try_from(0).unwrap(), MarketStatus::Uninitialized);
        assert_eq!(MarketStatus::try_from(1).unwrap(), MarketStatus::Open);
        assert_eq!(MarketStatus::try_from(2).unwrap(), MarketStatus::Closed);
        assert_eq!(MarketStatus::try_from(3).unwrap(), MarketStatus::Settled);
        assert_eq!(MarketStatus::try_from(4).unwrap(), MarketStatus::Cancelled);
        assert!(MarketStatus::try_from(5).is_err());
    }
}
