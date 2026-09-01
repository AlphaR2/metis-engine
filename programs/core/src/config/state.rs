use bytemuck::{Pod, Zeroable};
use pinocchio::Address;

/// One singleton account, protocol-wide, shared by every market.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Config {
    /// The market's opening seed fee, split evenly across outcomes at
    /// initialize_market.
    pub market_init_fee: u64,
    /// Per-trade fee taken on place_bet, in basis points.
    pub fee_bps: u16,
    /// Global slippage ceiling, in basis points: the maximum a single trade
    /// is allowed to move the price away from the naive flat-price estimate,
    /// enforced for every bettor to protect them from Slippage
    pub slippage_limit_bps: u16,
    /// Option A from the max_in design: a single trade can never exceed
    /// this fraction of the vault's current balance, in basis points.
    /// Scales automatically with pool depth
    pub max_in_bps_of_vault: u16,
    /// Intended to be a Squads multisig vault for admin controls.
    ///
    /// TODO(squads-multisig)
    /// not started.
    pub admin: Address,
    /// The stablecoin every NEW market must be denominated in (USDC).
    ///
    /// Markets snapshot this into `Market.quote_mint` at creation and stay
    /// bound to it for life — a token account's mint is immutable once
    /// initialized, so changing this only affects markets created afterwards.
    /// Live markets keep whatever they opened with.
    pub quote_mint: Address,
    pub discriminator: [u8; 8],
    pub max_outcomes: u8,
    pub bump: u8,
}

const _: () = assert!(core::mem::size_of::<Config>() == 88);
const _: () = assert!(core::mem::align_of::<Config>() == 8);

// ── Discriminators owned by this part
// Do not hand-edit. Regenerate with `cargo run --bin compute-discriminators`
// if a struct is renamed or a new account/instruction is added.

pub const CONFIG_DISCRIMINATOR: [u8; 8] = [155, 12, 170, 224, 30, 250, 204, 130];
pub const INITIALIZE_CONFIG: [u8; 8] = [208, 127, 21, 1, 194, 190, 196, 70];
pub const UPDATE_CONFIG: [u8; 8] = [29, 158, 252, 191, 10, 83, 219, 99];
