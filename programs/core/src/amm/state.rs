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
    /// A DBC-backed market whose raise has not been activated yet: no bets, no seed shares.
    Pending = 5,
}

impl MarketStatus {
    /// Unlike oracle/'s CFootballMatchPhase::can_transition_to, this is NOT idempotent-retry-
    /// friendly on purpose: re-settling an already-Settled market must never silently succeed a
    /// second time (unlike an oracle score re-send, a repeat settlement is a correctness risk,
    /// not a harmless retry).
    ///
    ///   Pending ──activate──▶ Open               (the raise graduated and funded the vault)
    ///   Pending ──cancel_pending──▶ Cancelled    (the raise missed its deadline; backers exit on the curve)
    ///   Open ──close_primary──▶ Closed
    ///   Open / Closed ──settle──▶ Settled        (terminal)
    ///   Open / Closed ──cancel──▶ Cancelled      (terminal)
    ///
    /// Closed is optional: settlement doesn't depend on anyone having called close_primary, only
    /// the order books and the revoked mint authorities do.
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Open, Self::Closed)
                | (Self::Open, Self::Settled)
                | (Self::Closed, Self::Settled)
                | (Self::Open, Self::Cancelled)
                | (Self::Closed, Self::Cancelled)
                | (Self::Pending, Self::Open)
                | (Self::Pending, Self::Cancelled)
        )
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
            5 => Ok(MarketStatus::Pending),
            _ => Err(ProgramError::InvalidAccountData),
        }
    }
}

/// How a market was seeded. Recorded so settlement knows who the seed's fee share belongs to.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedSource {
    /// The creator paid the seed and holds the seed shares (the default, and the only mode
    /// while Config.dbc_backing_enabled is off).
    Creator = 0,
    /// A DBC backer raise paid the seed; backers redeem against the seed shares.
    DbcBackers = 1,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Market {
    /// Real shares minted per outcome (seed + bets). Payouts divide by these, never by
    /// anything virtual.
    pub outcome_shares: [u64; MAX_OUTCOMES],
    /// The virtual liquidity offset per side, frozen at creation from Config's k.
    pub virtual_per_side: u64,
    /// The pot: the seed plus every bet net of its fee. Payouts come out of this.
    pub total_collected: u64,
    /// Fees taken from bets, held in the vault beside the pot and swept at settlement.
    pub fees_accrued: u64,
    /// What winners share once settled (the pot minus the rake), or what holders are refunded
    /// once cancelled (everything in the vault, fees included). Zero before either.
    pub payout_pool: u64,
    /// Set at settlement: what the protocol (the rake plus its share of fees) and the seed
    /// holder (the rest of the fees) may sweep out of the vault. Paid by sweep_fees, never inside
    /// settlement itself - so a closed or hostile recipient account can't block anyone's claim.
    pub protocol_owed: u64,
    pub creator_owed: u64,
    /// The platform's cut of the revenue, owed once settled. Paid to `platform` by sweep_fees.
    pub platform_owed: u64,
    /// The backers' cut, owed once settled on a DBC-backed market. Always zero on a
    /// creator-seeded market, where the backers' share is folded into `creator_owed`.
    pub backers_owed: u64,
    /// Bookkeeping for creator fees that went unclaimed past CREATOR_CLAIM_SECS and were forfeited
    /// to the treasury by two roots: how much, and when. Zero unless that happened. The same
    /// facts are in the forfeiting transaction's log, so they outlive the market's own account.
    pub forfeited_creator_fees: u64,
    pub forfeited_at: i64,
    /// DBC-backed markets only (zero otherwise): the seed the raise must deliver into the vault
    /// before the market may open, in USDC atoms.
    pub seed_target: u64,
    /// DBC-backed markets only: the backer token's supply when the market opened. Backers redeem
    /// their share as `tokens burned / supply still outstanding`, so no per-backer state exists.
    pub backer_supply: u64,
    /// DBC-backed markets only: the raise must activate the market by this time or it may be
    /// cancelled and backers sell back on the curve.
    pub raise_deadline: i64,
    /// Kickoff, as a unix timestamp. Taken from the oracle at creation.
    pub bet_close_ts: i64,
    /// Betting stops here (kickoff - PRIMARY_WINDOW_SECS); after it, trading moves to the books.
    pub primary_end: i64,
    /// When the market was settled or cancelled. Zero before either. The claim window runs
    /// from here.
    pub resolved_at: i64,
    /// The creator-chosen nonce this market's PDA was derived from.
    pub nonce: u64,
    /// Snapshots of Config's dials at creation. A later Config change never touches a live
    /// market.
    pub dispute_secs: u32,
    pub fee_bps: u16,
    pub rake_bps: u16,
    pub fee_share_protocol_bps: u16,
    pub fee_share_backers_bps: u16,
    pub fee_share_creator_bps: u16,
    pub fee_share_platform_bps: u16,
    /// Creator of the market. Permissionless, anyone can create a market.
    pub creator: Address,
    /// The product the market was created through (the customer embedding Metis), which earns
    /// `fee_share_platform_bps` of the revenue. All zeroes means none, and that share stays with
    /// the protocol. Named by the creator at creation; a creator may name themselves.
    pub platform: Address,
    /// The oracle account this market settles against.
    pub oracle: Address,
    /// This market's USDC vault: an SPL token account whose authority is the market PDA.
    pub vault: Address,
    /// The stablecoin this market is denominated in, snapshotted from Config.
    pub quote_mint: Address,
    /// DBC-backed markets only: the backer token (the DBC base mint). All zeroes otherwise.
    pub backer_mint: Address,
    pub discriminator: [u8; 8],
    /// How many of `outcome_shares` are in play. Always <= MAX_OUTCOMES.
    pub num_outcomes: u8,
    /// MarketStatus as u8.
    pub status: u8,
    /// Index into outcome_shares, or OUTCOME_UNRESOLVED.
    pub winning_outcome: u8,
    pub bump: u8,
    /// What makes outcome 0 ("YES") win, judged from the oracle's result. Outcome 1 ("NO")
    /// wins whenever it doesn't, so every market stays binary.
    pub condition_kind: u8,
    /// Parameter for condition kinds that need one (e.g. the goal line). Zero otherwise.
    pub condition_param: u8,
    /// SeedSource as u8.
    pub seed_source: u8,
    pub _padding: [u8; 1],
}

const _: () = assert!(core::mem::size_of::<Market>() == 376);
const _: () = assert!(core::mem::align_of::<Market>() == 8);

// ── Discriminators owned by this part
// Do not hand-edit. Regenerate with `cargo run --bin compute-discriminators`
// if a struct is renamed or a new account/instruction is added.
pub const MARKET_DISCRIMINATOR: [u8; 8] = [219, 190, 213, 55, 0, 227, 198, 154];
pub const INITIALIZE_MARKET: [u8; 8] = [35, 35, 189, 193, 155, 48, 170, 203];
pub const PLACE_BET: [u8; 8] = [222, 62, 67, 220, 63, 166, 126, 33];
pub const SET_METADATA: [u8; 8] = [78, 157, 75, 242, 151, 20, 121, 144];
pub const CLOSE_PRIMARY: [u8; 8] = [82, 72, 216, 96, 241, 39, 119, 211];
pub const INITIALIZE_BACKED_MARKET: [u8; 8] = [231, 227, 24, 84, 60, 60, 80, 227];
pub const ACTIVATE_MARKET: [u8; 8] = [10, 26, 197, 116, 113, 99, 72, 89];

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
            outcome_shares: [7, 9],
            virtual_per_side: 424,
            total_collected: 16,
            fees_accrued: 3,
            payout_pool: 0,
            protocol_owed: 0,
            creator_owed: 0,
            platform_owed: 0,
            backers_owed: 0,
            forfeited_creator_fees: 0,
            forfeited_at: 0,
            seed_target: 0,
            backer_supply: 0,
            raise_deadline: 0,
            bet_close_ts: 1_800_000_000,
            primary_end: 1_800_000_000 - crate::constants::PRIMARY_WINDOW_SECS,
            resolved_at: 0,
            nonce: 42,
            dispute_secs: 900,
            fee_bps: 50,
            rake_bps: 100,
            fee_share_protocol_bps: 4_000,
            fee_share_backers_bps: 3_000,
            fee_share_creator_bps: 2_000,
            fee_share_platform_bps: 1_000,
            creator: Default::default(),
            platform: Default::default(),
            oracle: Default::default(),
            vault: Default::default(),
            quote_mint: Default::default(),
            backer_mint: Default::default(),
            discriminator: MARKET_DISCRIMINATOR,
            num_outcomes: 2,
            status: MarketStatus::Open as u8,
            winning_outcome: crate::constants::OUTCOME_UNRESOLVED,
            bump: 254,
            condition_kind: 0,
            condition_param: 0,
            seed_source: SeedSource::Creator as u8,
            _padding: [0; 1],
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
            (5, MarketStatus::Pending),
        ] {
            let got = MarketStatus::try_from(n).unwrap();
            println!("status byte {n} -> {got:?}");
            assert_eq!(got, expected);
        }

        // Anything outside the known set is rejected rather than silently
        // treated as some valid state.
        println!("status byte 6 -> rejected (as it should be)");
        assert!(MarketStatus::try_from(6).is_err());
    }

    // The exact set of legal transitions, checked exhaustively rather than spot-checked, since
    // this is the guard settlement's whole solvency story rests on - including the cases that
    // matter most: Settled and Cancelled are terminal, and neither can follow the other.
    #[test]
    fn market_status_can_transition_to_is_exhaustive() {
        use MarketStatus::*;
        let all = [Uninitialized, Open, Closed, Settled, Cancelled, Pending];
        let legal = [
            (Open, Closed),
            (Open, Settled),
            (Closed, Settled),
            (Open, Cancelled),
            (Closed, Cancelled),
            (Pending, Open),
            (Pending, Cancelled),
        ];

        for from in all {
            for to in all {
                let allowed = from.can_transition_to(to);
                println!("{from:?} -> {to:?}: {allowed}");
                assert_eq!(allowed, legal.contains(&(from, to)), "unexpected legality for {from:?} -> {to:?}");
            }
        }
    }
}
