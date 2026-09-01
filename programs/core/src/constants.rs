use solana_address::{Address, address};
pub const MARKET_SEED: &[u8] = b"market";
pub const POSITION_SEED: &[u8] = b"position";
pub const VAULT_SEED: &[u8] = b"vault";
pub const CONFIG_SEED: &[u8] = b"config";
pub const OUTCOME_MINT_SEED: &[u8] = b"outcome_mint";

/// Outcome tokens use the same decimals as the quote mint (USDC = 6), which
/// is what lets MARKET_INIT_FEE and the curve's share scale line up 1:1.
pub const QUOTE_DECIMALS: u8 = 6;

// Outcome encoding
// N is a dial: bump it and every struct below resizes with it.
// Valid as long as MAX_OUTCOMES stays well under 255

pub const MAX_OUTCOMES: usize = 2;
pub const OUTCOME_UNRESOLVED: u8 = u8::MAX; // highest u8 value, used as "no winner yet"

// v1: fixed protocol-wide, not creator-chosen. Every market pays the same
// fee and shares the same authority. Revisit if that ever needs to vary.
pub const MARKET_INIT_FEE: u64 = 10_000_000; // TODO: confirm unit — see note below
pub const PRIMARY_WINDOW_SECS: i64 = 5 * 60 * 60; // 5 hours before kickoff --  can be changed.  

// TODO(single-point-of-failure): see the full note on Market.authority in
// state/market.rs — this constant is the thing planned to go away in favor
// of quorum-based settlement. Deferred, not designed yet.
//
// TODO: placeholder — keypair lives at keys/authority.json, gitignored.
// #[cfg(test) can't split this: cargo build-sbf never compiles with
// --cfg test, so the deployed .so always bakes in the cfg(not(test))
// branch regardless of what cargo test sees. A test-only value here is
// silently ignored by the actual on-chain binary. Swap this for a real
// production key (and a real keypair to match) before any real deploy.
pub const AUTHORITY: Address = address!("eaZoYS1Ej58sL64S3PtZbo7XS5euS7oTC1CzsEmP6VE");

// Gates the ONE-TIME initialize_config call only — whoever controls this
// key is the only one who can create the singleton Config account, which
// then records its own `admin` field for every subsequent update_config
// call. Without this gate, initialize_config would be a race: whoever's
// transaction lands first becomes admin forever, permissionlessly, which is
// not the intent. Same placeholder-keypair caveat as AUTHORITY above —
// swap for the real bootstrap key (intended to be a Squads multisig vault,
// per the chosen auth model) before any real deploy. Once Config exists,
// this constant is never read again; all further admin checks read
// Config.admin, not this.
pub const INITIAL_CONFIG_ADMIN: Address = address!("eaZoYS1Ej58sL64S3PtZbo7XS5euS7oTC1CzsEmP6VE");
