use solana_address::{Address, address};
pub const MARKET_SEED: &[u8] = b"market";
pub const VAULT_SEED: &[u8] = b"vault";
pub const CONFIG_SEED: &[u8] = b"config";
pub const OUTCOME_MINT_SEED: &[u8] = b"outcome_mint";
pub const DELEGATED_AUTHORITY_SEED: &[u8] = b"delegated_authority";
pub const QUOTE_DECIMALS: u8 = 6;

// Outcome encoding
// N is a dial: bump it and every struct below resizes with it.
// Valid as long as MAX_OUTCOMES stays well under 255
pub const MAX_OUTCOMES: usize = 2;
pub const OUTCOME_UNRESOLVED: u8 = u8::MAX; // highest u8 value, used as "no winner yet"

// v0.1: For this version, we wil set minimum creator capital cap as a fixed protocol-wide, not creator-chosen. 
// Hence, Every market pays the same
pub const MIN_SEED: u64 = 20_000_000; // 20 USDC (6 decimals), no max
pub const PRIMARY_WINDOW_SECS: i64 = 5 * 60 * 60; // 5 hours before kickoff --  can be changed.

/// Curvature is fixed at r = 2 for mvp.
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

/// Ceiling on secondary-market (CLOB) taker fee.
pub const MAX_FEE_TAKER_BPS: u16 = 50;

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
const _: () = assert!(MAX_FEE_TAKER_BPS <= BPS_DENOMINATOR);
const _: () = assert!(MAX_PRICE_IMPACT_BPS <= BPS_DENOMINATOR);


// Swap this for a real
// production key (and a real keypair to match) before any real deploy.
// pub const AUTHORITY: Address = address!("eaZoYS1Ej58sL64S3PtZbo7XS5euS7oTC1CzsEmP6VE");

// Root authority keys - same 3 signers as oracle/, so both programs share one trust root

pub const ROOT_SIGNERS: [Address; 3] = [
    address!("UPDTR3Mmf4qQatb5qy8rE7aEw34Jq2AUJknEgbTuw8B"),
    address!("UPdtRERKVQhyEEFmxTUmmezz1BStBS1wiXn1h74bhRG"),
    address!("upDTrFkAvEEk2N9JoxbeY6jg1KHJNzu8BAqdf7f2xVc"),
];

// PLACEHOLDER — must be a real, independently-held keypair before any real deploy, distinct
// from all three ROOT_SIGNERS. Same address as oracle's GUARDIAN placeholder, same caveat.
pub const GUARDIAN: Address = address!("eaZoYS1Ej58sL64S3PtZbo7XS5euS7oTC1CzsEmP6VE");

// ── Cross-program references (oracle/) 
// NOTE: Fix with real value on initial deploy. 
// oracle is a separate program - reading its accounts is a plain byte-level read (no CPI
// needed for reads, only writes), but this program has no dependency on oracle's crate, so
// these values are independently declared here rather than imported. Must be kept in sync by
// hand with oracle/src/lib.rs's declare_id! and oracle/src/constants.rs's
// FOOTBALL_SETTLED_DISCRIMINATOR - same "shared logic, not shared data" principle as
// ROOT_SIGNERS/GUARDIAN above.
pub const ORACLE_PROGRAM_ID: Address = address!("PorC1q7btDTY3jyrNxCgieXU5a4nGzhchjjnThYM3un");
pub const ORACLE_FOOTBALL_DISCRIMINATOR: [u8; 8] = [130, 33, 121, 117, 222, 174, 224, 194];
