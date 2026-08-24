use solana_address::{address, Address};
pub const MARKET_SEED: &[u8] = b"market";
pub const POSITION_SEED: &[u8] = b"position";
pub const VAULT_SEED: &[u8] = b"vault";

// Outcome encoding
// N is a dial: bump it and every struct below resizes with it.
// Valid as long as MAX_OUTCOMES stays well under 255

pub const MAX_OUTCOMES: usize = 2;
pub const OUTCOME_UNRESOLVED: u8 = u8::MAX; // highest u8 value, used as "no winner yet"

// v1: fixed protocol-wide, not creator-chosen. Every market pays the same
// fee and shares the same authority. Revisit if that ever needs to vary.
pub const MARKET_INIT_FEE: u64 = 10_000_000; // TODO: confirm unit — see note below

// TODO: placeholder — keypair lives at keys/authority.json, gitignored.
// #[cfg(test) can't split this: cargo build-sbf never compiles with
// --cfg test, so the deployed .so always bakes in the cfg(not(test))
// branch regardless of what cargo test sees. A test-only value here is
// silently ignored by the actual on-chain binary. Swap this for a real
// production key (and a real keypair to match) before any real deploy.
pub const AUTHORITY: Address = address!("eaZoYS1Ej58sL64S3PtZbo7XS5euS7oTC1CzsEmP6VE");
