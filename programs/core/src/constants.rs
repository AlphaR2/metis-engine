#[cfg(all(feature = "mainnet", feature = "short-timers"))]
compile_error!("`short-timers` must never be combined with `mainnet`");

use solana_address::{Address, address};
pub const MARKET_SEED: &[u8] = b"market";
pub const VAULT_SEED: &[u8] = b"vault";
pub const CONFIG_SEED: &[u8] = b"config";
pub const OUTCOME_MINT_SEED: &[u8] = b"outcome_mint";
pub const DELEGATED_AUTHORITY_SEED: &[u8] = b"delegated_authority";
/// A market's two Manifest order books: [book, market, outcome_index]. Created by close_primary.
pub const BOOK_SEED: &[u8] = b"book";
/// A market's question, rubrics and labels: [metadata, market]. Created once, never rewritten.
pub const METADATA_SEED: &[u8] = b"metadata";
pub const QUOTE_DECIMALS: u8 = 6;

// Outcome encoding
// N is a dial: bump it and every struct below resizes with it.
// Valid as long as MAX_OUTCOMES stays well under 255
pub const MAX_OUTCOMES: usize = 2;
pub const OUTCOME_UNRESOLVED: u8 = u8::MAX; // highest u8 value, used as "no winner yet"

/// Hardcoded floor on Config.market_init_fee, the creator's seed. Config may raise the seed,
/// never lower it below this.
pub const MIN_SEED: u64 = 30_000_000; // 30 USDC (6 decimals), no max
#[cfg(not(feature = "short-timers"))]
pub const PRIMARY_WINDOW_SECS: i64 = 5 * 60 * 60; // 5 hours before kickoff --  can be changed.
/// Shortened for the local rehearsal build only (see the `localnet` feature), so a whole
/// market life fits in minutes. Every other build uses the real value above.
#[cfg(feature = "short-timers")]
pub const PRIMARY_WINDOW_SECS: i64 = 120;

/// Curvature is fixed at r = 2 for mvp.
/// A DBC-backed market's seed may not exceed this (100,000 USDC): a sanity bound, not a product limit.
pub const MAX_BACKED_SEED: u64 = 100_000_000_000;

/// How long a DBC raise may run before the market it seeds must open or be cancelled.
#[cfg(not(feature = "short-timers"))]
pub const MIN_RAISE_SECS: i64 = 20 * 60;
#[cfg(feature = "short-timers")]
pub const MIN_RAISE_SECS: i64 = 60;
#[cfg(not(feature = "short-timers"))]
pub const MAX_RAISE_SECS: i64 = 2 * 60 * 60;
#[cfg(feature = "short-timers")]
pub const MAX_RAISE_SECS: i64 = 600;

pub const CURVATURE_R: u8 = 2;

/// Virtual-liquidity multiple k (docs/mvp.md 1.4): virtual_per_side =
/// k × (seed / 2^(1/r)).
/// Employed Virtual Liq as per research from pump fun etc. 
/// 30 is a reasonable starting point for the MVP, but this will
/// be tuned in the future. The value of k is a tradeoff between liquidity and
/// price sensitivity. A higher k means more liquidity but less price
/// sensitivity, while a lower k means less liquidity but more price
/// sensitivity. The value of k should be chosen based on the specific use
/// case and the desired balance between liquidity and price sensitivity.
pub const VIRTUAL_LIQUIDITY_MULTIPLE: u64 = 30;

/// Ceiling on Config.virtual_liquidity_multiple. An admin picking k too high
/// would make every market artificially deep and insensitive to real money;
/// this bounds how far that dial can be pushed. Same role MAX_FEE_PRIMARY_BPS
/// plays for fee_bps.
pub const MAX_VIRTUAL_LIQUIDITY_MULTIPLE: u64 = 100;

const _: () = assert!(VIRTUAL_LIQUIDITY_MULTIPLE <= MAX_VIRTUAL_LIQUIDITY_MULTIPLE);

// ── Basis-point bounds for validation
// bps fields on Config are validated against these, not against a bare
// 10_000, so "mathematically valid" and "not predatory" stay two separate,
// visible checks instead of one magic number doing both jobs.
/// 100.00% in basis points.
pub const BPS_DENOMINATOR: u16 = 10_000;

/// Ceiling on Config.fee_bps — the fee taken on place_bet (AMM/primary
/// market). Fees are a tunable business lever, but an admin (or a
/// compromised admin key) should never be able to set one that eats a
/// meaningful fraction of every trade. 50 = 0.50%.
pub const MAX_FEE_PRIMARY_BPS: u16 = 50;

/// Ceiling on Config.rake_bps - the cut taken from the pot at settlement. 300 = 3.00%.
pub const MAX_RAKE_BPS: u16 = 300;

/// Ceiling on the protocol's share of a market's fees. The rest goes to the seed holder and the
/// creator; a compromised root key can't route every fee to the treasury. 5_000 = 50.00%.
pub const MAX_PROTOCOL_SHARE_BPS: u16 = 5_000;

/// Ceiling on Config.max_in_bps_of_vault. Mirrors docs/research.md 3.4's
/// "10 pp hard ceiling, never exceeded by any reachable trade" 
/// 1000 = 10.00%. 
/// 
/// With 15% of the vault being the max and must not be practically hit anywhere in the system. 
/// This means that a single trade can never move more than 10% of the vault, 
/// which is a reasonable limit to prevent market manipulation and ensure liquidity. 
/// This limit is also in line with industry standards for decentralized exchanges and automated market makers, where large trades can have a significant impact on prices and liquidity. 
/// 
/// By setting a hard ceiling on the maximum percentage of the vault that can be used in a single trade, we can help maintain a fair and stable trading environment for all users.
pub const MAX_PRICE_IMPACT_BPS: u16 = 1_000;

const _: () = assert!(MAX_FEE_PRIMARY_BPS <= BPS_DENOMINATOR);
const _: () = assert!(MAX_RAKE_BPS <= BPS_DENOMINATOR);
const _: () = assert!(MAX_PROTOCOL_SHARE_BPS <= BPS_DENOMINATOR);

// ── Time bounds
/// Bounds on Config.dispute_secs: how long after the oracle finalises a result settlement must
/// wait, so a bad result can still be overridden. Too short and there is no time to react; too
/// long and winners wait for nothing.
#[cfg(not(feature = "short-timers"))]
pub const MIN_DISPUTE_SECS: u32 = 15 * 60;
/// Shortened for the local rehearsal build only (see the `localnet` feature), so a whole
/// market life fits in minutes. Every other build uses the real value above.
#[cfg(feature = "short-timers")]
pub const MIN_DISPUTE_SECS: u32 = 20;
pub const MAX_DISPUTE_SECS: u32 = 24 * 60 * 60;

/// No finalised result this long after kickoff and anyone can cancel the market, so funds are
/// never stuck behind an oracle that stopped reporting.
#[cfg(not(feature = "short-timers"))]
pub const CANCEL_AFTER_SECS: i64 = 7 * 24 * 60 * 60;
/// Shortened for the local rehearsal build only (see the `localnet` feature), so a whole
/// market life fits in minutes. Every other build uses the real value above.
#[cfg(feature = "short-timers")]
pub const CANCEL_AFTER_SECS: i64 = 600;

/// How long after settlement or cancellation holders have to claim before the market can be
/// closed and its dust swept.
#[cfg(not(feature = "short-timers"))]
pub const CLAIM_WINDOW_SECS: i64 = 90 * 24 * 60 * 60;
/// Shortened for the local rehearsal build only (see the `localnet` feature), so a whole
/// market life fits in minutes. Every other build uses the real value above.
#[cfg(feature = "short-timers")]
pub const CLAIM_WINDOW_SECS: i64 = 300;

/// How long an operator has to act before the instructions with a fallback open to anyone.
///
/// Settling and cancelling are gated to operators, but they protect holders' money, and a market
/// must never be stuck behind an operator that has vanished. So once one of them has been
/// possible for this long and nobody has done it, it becomes permissionless again. (Opening the
/// order books, sweeping the protocol's fees and closing stay gated for good: nobody's money
/// depends on them.)
#[cfg(not(feature = "short-timers"))]
pub const OPERATOR_GRACE_SECS: i64 = 7 * 24 * 60 * 60;
/// Shortened for the local rehearsal build only (see the `localnet` feature), so a whole
/// market life fits in minutes. Every other build uses the real value above.
#[cfg(feature = "short-timers")]
pub const OPERATOR_GRACE_SECS: i64 = 120;

/// How long a market's creator has to claim their fees after settlement. Then two roots may
/// forfeit what is left to the treasury (forfeit_creator_fees), with a record kept on the market
/// and in the transaction's log so a manual settlement is always possible.
#[cfg(not(feature = "short-timers"))]
pub const CREATOR_CLAIM_SECS: i64 = 30 * 24 * 60 * 60;
/// Shortened for the local rehearsal build only (see the `localnet` feature), so a whole
/// market life fits in minutes. Every other build uses the real value above.
#[cfg(feature = "short-timers")]
pub const CREATOR_CLAIM_SECS: i64 = 180;

const _: () = assert!(MIN_DISPUTE_SECS <= MAX_DISPUTE_SECS);
const _: () = assert!(MAX_PRICE_IMPACT_BPS <= BPS_DENOMINATOR);


// Root authority keys and guardian. One source for every Metis program, chosen per network by
// the `devnet` / `mainnet` features - see authority/src/network.rs.
pub use metis_authority::network::{GUARDIAN, ROOT_SIGNERS};

// ── Cross-program references
// The oracle's program id and account discriminator come from the oracle crate itself
// (`oracle::ID`, `oracle::constants::FOOTBALL_SETTLED_DISCRIMINATOR`) - core depends on it, so
// the two can never be out of step.

/// Manifest's core order-book program (Bonasa-Tech/manifest). Outcome tokens trade there after
/// the primary window; close_primary creates the books by CPI.
pub const MANIFEST_PROGRAM_ID: Address = address!("MNFSTqtC93rEfYHB6hF82sKdZpUDFWkViLByLd1k1Ms");
