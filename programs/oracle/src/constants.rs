#[cfg(all(feature = "mainnet", feature = "short-timers"))]
compile_error!("`short-timers` must never be combined with `mainnet`");

//PDA seeds 

pub const AUTHORITY_SEED: &[u8] = b"oracle_authority";
pub const INDIVIDUAL_AUTHORITY_SEED: &[u8] = b"individual_authority";

pub const FOOTBALL_SEED: &[u8] = b"football";
pub const SETTLED_SEED: &[u8] = b"settled";
pub const CLOSER_QUORUM_SEED: &[u8] = b"closer_quorum";

// Root authority keys and guardian. One source for every Metis program, chosen per network by
// the `devnet` / `mainnet` features - see authority/src/network.rs. The guardian used to be a
// copy of the third root signer here, which defeated its purpose; it is now a distinct key,
// enforced at compile time.
pub use metis_authority::network::{GUARDIAN, ROOT_SIGNERS};

pub const FOOTBALL_SETTLED_DISCRIMINATOR:   [u8; 8] = [130, 33, 121, 117, 222, 174, 224, 194];
pub const ORACLE_AUTHORITY_DISCRIMINATOR:   [u8; 8] = [5, 121, 209, 230, 34, 112, 66, 218];
pub const INDIVIDUAL_AUTHORITY_DISCRIMINATOR: [u8; 8] = [45, 109, 39, 46, 149, 138, 115, 231];

// ── Result reporting
//
// Hardcoded on purpose: none of these may be moved by a root key, because they are what stops a
// single reporter (or a single compromised key) from deciding a result.

/// Distinct reporters that must submit the same result before it is applied.
pub const REQUIRED_REPORTS: usize = 2;

/// Reporter slots per fixture - one per data provider (the keeper runs 2-3).
pub const MAX_REPORTERS: usize = 3;

const _: () = assert!(REQUIRED_REPORTS >= 2 && REQUIRED_REPORTS <= MAX_REPORTERS);

/// A fixture must be created at least this far before kickoff, so an oracle can never be born
/// for a match that is already under way. (Markets enforce the longer primary window
/// themselves, at creation.)
#[cfg(not(feature = "short-timers"))]
pub const MIN_KICKOFF_LEAD_SECS: i64 = 60 * 60;
/// Shortened for the local rehearsal build only (see the `localnet` feature), so a whole
/// market life fits in minutes. Every other build uses the real value above.
#[cfg(feature = "short-timers")]
pub const MIN_KICKOFF_LEAD_SECS: i64 = 30;

/// After a result is finalised, two roots may still override or void it for this long. Longer
/// than the longest dispute window a market can choose, so every market's dispute window sits
/// inside it.
#[cfg(not(feature = "short-timers"))]
pub const OVERRIDE_WINDOW_SECS: i64 = 24 * 60 * 60;
/// Shortened for the local rehearsal build only (see the `localnet` feature), so a whole
/// market life fits in minutes. Every other build uses the real value above.
#[cfg(feature = "short-timers")]
pub const OVERRIDE_WINDOW_SECS: i64 = 120;

/// How long a finalised or voided oracle (or one that never finished) is kept before it can be
/// closed and its rent reclaimed - long enough for every market on it to settle or cancel.
#[cfg(not(feature = "short-timers"))]
pub const CLOSE_GRACE_SECS: i64 = 30 * 24 * 60 * 60;
/// Shortened for the local rehearsal build only (see the `localnet` feature), so a whole
/// market life fits in minutes. Every other build uses the real value above.
#[cfg(feature = "short-timers")]
pub const CLOSE_GRACE_SECS: i64 = 300;

// ── Football score bitpacking ────────────────────────────────────────────

/// A single home/away score nibble never exceeds this - the packed byte
/// gives each side 4 bits (0..=15).
pub const SCORE_NIBBLE_MAX: u8 = 15;

/// Mask for one 4-bit nibble within a packed score/penalties byte.
pub const SCORE_NIBBLE_MASK: u8 = 0x0F;

const _: () = assert!(SCORE_NIBBLE_MAX == SCORE_NIBBLE_MASK);
