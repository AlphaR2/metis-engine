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
