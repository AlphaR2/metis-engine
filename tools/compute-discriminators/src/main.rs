// Dev tool, not part of the program. Run it, paste the output into
// discriminator.rs, done. Never invoked on-chain — that's why sha2 is only
// a dependency here, not in the program's own build.
//
//   cargo run --bin compute-discriminators

use sha2::{Digest, Sha256};

// First 8 bytes of sha256(preimage). Anchor's own convention, so any
// Anchor client / Codama SDK can derive the same bytes independently
// just from knowing the account or instruction name.
fn discriminator(preimage: &str) -> [u8; 8] {
    let hash = Sha256::digest(preimage.as_bytes());
    hash[0..8].try_into().unwrap()
}

fn main() {
    // add more when you add more accounts or instructions, and paste the output into discriminator.rs

    println!("// ── Account discriminators ──────────────────────────────────────────");
    println!(
        "pub const MARKET_DISCRIMINATOR:   [u8; 8] = {:?};",
        discriminator("account:Market")
    );
    println!(
        "pub const POSITION_DISCRIMINATOR: [u8; 8] = {:?};",
        discriminator("account:Position")
    );
    println!(
        "pub const VAULT_DISCRIMINATOR:    [u8; 8] = {:?};",
        discriminator("account:Vault")
    );
    println!(
        "pub const CONFIG_DISCRIMINATOR:   [u8; 8] = {:?};",
        discriminator("account:Config")
    );
    println!(
        "pub const DELEGATED_AUTHORITY_DISCRIMINATOR: [u8; 8] = {:?};",
        discriminator("account:DelegatedAuthority")
    );
    println!(
        "pub const MARKET_METADATA_DISCRIMINATOR: [u8; 8] = {:?};",
        discriminator("account:MarketMetadata")
    );

    println!();
    println!("// ── Instruction discriminators ───────────────────────────────────────");
    for name in ["initialize_backed_market", "activate_market", "claim_backer", "cancel_pending_market"] {
        println!("pub const {}: [u8; 8] = {:?};", name.to_uppercase(), discriminator(&format!("global:{name}")));
    }

    println!(
        "pub const INITIALIZE_MARKET: [u8; 8] = {:?};",
        discriminator("global:initialize_market")
    );
    println!(
        "pub const PLACE_BET:         [u8; 8] = {:?};",
        discriminator("global:place_bet")
    );
    println!(
        "pub const SETTLE_MARKET:     [u8; 8] = {:?};",
        discriminator("global:settle_market")
    );
    println!(
        "pub const CLAIM:             [u8; 8] = {:?};",
        discriminator("global:claim")
    );
    println!(
        "pub const INITIALIZE_CONFIG: [u8; 8] = {:?};",
        discriminator("global:initialize_config")
    );
    println!(
        "pub const UPDATE_CONFIG:     [u8; 8] = {:?};",
        discriminator("global:update_config")
    );
    println!(
        "pub const ATTEST_AUTHORITY:  [u8; 8] = {:?};",
        discriminator("global:attest_authority")
    );
    println!(
        "pub const REMOVE_AUTHORITY:  [u8; 8] = {:?};",
        discriminator("global:remove_authority")
    );
    for (name, ix) in [
        ("UPDATE_CONFIG_CRITICAL", "update_config_critical"),
        ("PAUSE", "pause"),
        ("UNPAUSE", "unpause"),
        ("CLOSE_PRIMARY", "close_primary"),
        ("SET_METADATA", "set_metadata"),
        ("SWEEP_FEES", "sweep_fees"),
        ("CANCEL_MARKET", "cancel_market"),
        ("CLAIM_REFUND", "claim_refund"),
        ("CLOSE_MARKET", "close_market"),
        ("FORFEIT_CREATOR_FEES", "forfeit_creator_fees"),
    ] {
        println!(
            "pub const {name}: [u8; 8] = {:?};",
            discriminator(&format!("global:{ix}"))
        );
    }

    // oracle/ account discriminators - paste into oracle/src/constants.rs
    println!();
    println!("// ── Oracle account discriminators ────────────────────────────────────");
    println!(
        "pub const FOOTBALL_SETTLED_DISCRIMINATOR:   [u8; 8] = {:?};",
        discriminator("account:FootballSettled")
    );
    println!(
        "pub const ORACLE_AUTHORITY_DISCRIMINATOR:   [u8; 8] = {:?};",
        discriminator("account:OracleAuthority")
    );
    println!(
        "pub const INDIVIDUAL_AUTHORITY_DISCRIMINATOR: [u8; 8] = {:?};",
        discriminator("account:IndividualOAuthority")
    );
}
