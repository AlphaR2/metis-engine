// Each value is the first 8 bytes of sha256("account:<Name>") — Anchor's own
// convention, so any Anchor client or Codama-generated SDK can derive these
// independently just from the struct name. Checked before trusting any other
// field on a Market/Position/Vault read, so a wrong-typed account is rejected
// immediately instead of silently misread.
//
// Do not hand-edit. Regenerate with `cargo run --bin compute-discriminators`
// if a struct is renamed or a new account type is added.

// ── Account discriminators ──────────────────────────────────────────
pub const MARKET_DISCRIMINATOR:   [u8; 8] = [219, 190, 213, 55, 0, 227, 198, 154];
pub const POSITION_DISCRIMINATOR: [u8; 8] = [170, 188, 143, 228, 122, 64, 247, 208];
pub const VAULT_DISCRIMINATOR:    [u8; 8] = [211, 8, 232, 43, 2, 152, 117, 119];

// ── Instruction discriminators ───────────────────────────────────────
pub const INITIALIZE_MARKET: [u8; 8] = [35, 35, 189, 193, 155, 48, 170, 203];
pub const PLACE_BET:         [u8; 8] = [222, 62, 67, 220, 63, 166, 126, 33];
pub const SETTLE_MARKET:     [u8; 8] = [193, 153, 95, 216, 166, 6, 144, 217];
pub const CLAIM:             [u8; 8] = [62, 198, 214, 193, 213, 159, 108, 210];