// There is no Vault struct any more. A market's vault is a plain SPL token
// account holding USDC, with the market PDA as its authority - the Token
// program already records its mint, owner and balance, so a separate account
// of our own would just be duplicating that. The market's `vault` field is
// what ties the two together.

// ── Discriminators owned by this part
// Do not hand-edit. Regenerate with `cargo run --bin compute-discriminators`
// if a struct is renamed or a new account/instruction is added.
pub const SETTLE_MARKET: [u8; 8] = [193, 153, 95, 216, 166, 6, 144, 217];
pub const CLAIM: [u8; 8] = [62, 198, 214, 193, 213, 159, 108, 210];
pub const SWEEP_FEES: [u8; 8] = [175, 225, 98, 71, 118, 66, 34, 148];
pub const CANCEL_MARKET: [u8; 8] = [205, 121, 84, 210, 222, 71, 150, 11];
pub const CLAIM_REFUND: [u8; 8] = [15, 16, 30, 161, 255, 228, 97, 60];
pub const FORFEIT_CREATOR_FEES: [u8; 8] = [209, 158, 142, 212, 116, 83, 163, 116];
pub const CLOSE_MARKET: [u8; 8] = [88, 154, 248, 186, 48, 14, 123, 244];
pub const CLAIM_BACKER: [u8; 8] = [172, 148, 83, 118, 116, 185, 190, 133];
pub const CANCEL_PENDING_MARKET: [u8; 8] = [187, 109, 220, 223, 176, 26, 179, 80];
