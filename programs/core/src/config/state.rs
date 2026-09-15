use bytemuck::{CheckedBitPattern, NoUninit};
use pinocchio::Address;
use metis_authority::root::RootAuthority;

/// One singleton account, protocol-wide, shared by every market.
#[repr(C)]
#[derive(Clone, Copy, Debug, NoUninit, CheckedBitPattern)]
pub struct Config {
    /// The market's opening seed fee that is paid for the creator of the market, 
    /// split evenly across outcomes at initialize_market.
    pub market_init_fee: u64,
    /// docs/mvp.md 1.4's k: virtual_per_side = k × symmetric_seed(fee, outcomes),
    /// computed once and frozen on each Market at initialize_market. Like
    /// quote_mint, changing this only affects markets created after the
    /// change — a live market's own virtual_per_side never moves.
    pub virtual_liquidity_multiple: u64,
    /// Per-trade fee taken on place_bet (primary/AMM market)
    pub fee_bps: u16,
    /// Option A from the max_in design: a single trade can never exceed
    /// this fraction of the vault's current balance, in basis points.
    /// Scales automatically with pool depth.
    pub max_in_bps_of_vault: u16,
    /// The root authority is the only entity that can update this config.
    pub root: RootAuthority,
    /// The stablecoin every NEW market must be denominated in (USDC).
    ///
    /// Markets snapshot this into `Market.quote_mint` at creation and stay
    /// bound to it for life. A token account's mint is immutable once
    /// initialized, so changing this only affects markets created afterwards.
    /// Live markets keep whatever they opened with.
    pub quote_mint: Address,
    /// Identifies this account as a Config to every instruction that reads
    /// it, so a Market or other account can never be misread as one.
    pub discriminator: [u8; 8],
    /// Admin's chosen ceiling on outcomes per new market. Must stay
    /// `<= constants::MAX_OUTCOMES` — that constant is the hard limit baked
    /// into Market's account layout ([u64; MAX_OUTCOMES]); this field is
    /// the admin's operating choice within it, which may be lower.
    pub max_outcomes: u8,
    /// This account's own PDA bump, cached at initialize_config so later
    /// instructions never need to re-derive it.
    pub bump: u8,
}


const _: () = assert!(core::mem::size_of::<Config>() == 200);
const _: () = assert!(core::mem::align_of::<Config>() == 8);

// ── Discriminators owned by this part
// Do not hand-edit. Regenerate with `cargo run --bin compute-discriminators`
// if a struct is renamed or a new account/instruction is added.

pub const CONFIG_DISCRIMINATOR: [u8; 8] = [155, 12, 170, 224, 30, 250, 204, 130];
pub const DELEGATED_AUTHORITY_DISCRIMINATOR: [u8; 8] = [89, 106, 242, 124, 175, 252, 140, 92];
pub const INITIALIZE_CONFIG: [u8; 8] = [208, 127, 21, 1, 194, 190, 196, 70];
pub const UPDATE_CONFIG: [u8; 8] = [29, 158, 252, 191, 10, 83, 219, 99];
pub const ATTEST_AUTHORITY: [u8; 8] = [109, 185, 129, 174, 230, 153, 126, 135];
pub const REMOVE_AUTHORITY: [u8; 8] = [242, 104, 208, 132, 190, 250, 74, 216];

impl metis_authority::instructions::RootBearing for Config {
    #[inline]
    fn root(&self) -> &RootAuthority {
        &self.root
    }

    #[inline]
    fn discriminator(&self) -> [u8; 8] {
        self.discriminator
    }
}
