use bytemuck::{Pod, Zeroable};
use pinocchio::{error::ProgramError, Address};
use crate::constants::{MAX_OUTCOMES};


// Market status

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketStatus {
    Uninitialized = 0,
    Open = 1,
    Settled = 2,
}

impl TryFrom<u8> for MarketStatus {
  type Error =  ProgramError;

  fn try_from(value: u8) -> Result<Self, Self::Error> {
    match value {
      0 => Ok(MarketStatus::Uninitialized),
      1 => Ok(MarketStatus::Open),
      2 => Ok(MarketStatus::Settled),
      _ => Err(ProgramError::InvalidAccountData)
    }
  }
}

//Accounts

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Market {
    /// Recorded shares, one per outcome.
    pub outcome_shares: [u64; MAX_OUTCOMES],
    /// Total money paid into the vault. Mirror of the vault's balance 
    pub total_collected: u64,
    /// Creator of the market. Permissionless, anyone can create a market
    pub creator: Address,
    /// Who may call settle. Also something to decide in V0.1
    pub authority: Address,
    /// This market's own vault. One vault per market, never shared.
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

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
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

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Vault {
    /// The market this vault belongs to.
    pub market: Address,
    pub discriminator: [u8; 8],
    pub bump: u8,
}


const _: () = assert!(core::mem::size_of::<Market>() == 136);
const _: () = assert!(core::mem::align_of::<Market>() == 8);
const _: () = assert!(core::mem::size_of::<Position>() == 96);
const _: () = assert!(core::mem::align_of::<Position>() == 8);
const _: () = assert!(core::mem::size_of::<Vault>() == 41);
const _: () = assert!(core::mem::align_of::<Vault>() == 1);
