use pinocchio::Address;

//PDA seeds 

pub const AUTHORITY_SEED: &[u8] = b"oracle_authority";
pub const INDIVIDUAL_AUTHORITY_SEED: &[u8] = b"individual_authority";

pub const FOOTBALL_SEED: &[u8] = b"football";
pub const SETTLED_SEED: &[u8] = b"settled";
pub const CLOSER_QUORUM_SEED: &[u8] = b"closer_quorum";

//  Root authority keys 
// note: each core part and program in metis comes with its own  root aut keys. 

// Swap these for real production keys (and real keypairs to match) before any real deploy.
// These are vanity "updater"-prefixed addresses, not audited production keys.
pub const ROOT_SIGNERS: [Address; 3] = [
    Address::from_str_const("UPDTR3Mmf4qQatb5qy8rE7aEw34Jq2AUJknEgbTuw8B"),
    Address::from_str_const("UPdtRERKVQhyEEFmxTUmmezz1BStBS1wiXn1h74bhRG"),
    Address::from_str_const("upDTrFkAvEEk2N9JoxbeY6jg1KHJNzu8BAqdf7f2xVc"),
];

// NOTE: Replace with actual keypair and address for the guardian. This is a vanity "guardian"-prefixed address, not an audited production key.
// The guardian is a separate key from the three ROOT_SIGNERS. Its only power anywhere in the system is co-signing Unpause; see metis_authority::RootAuthority's doc comment for why it has to stay structurally separate from root.
pub const GUARDIAN: Address = Address::from_str_const("upDTrFkAvEEk2N9JoxbeY6jg1KHJNzu8BAqdf7f2xVc");

pub const FOOTBALL_SETTLED_DISCRIMINATOR:   [u8; 8] = [130, 33, 121, 117, 222, 174, 224, 194];
pub const ORACLE_AUTHORITY_DISCRIMINATOR:   [u8; 8] = [5, 121, 209, 230, 34, 112, 66, 218];
pub const INDIVIDUAL_AUTHORITY_DISCRIMINATOR: [u8; 8] = [45, 109, 39, 46, 149, 138, 115, 231];

// ── Football score bitpacking ────────────────────────────────────────────

/// A single home/away score nibble never exceeds this - the packed byte
/// gives each side 4 bits (0..=15).
pub const SCORE_NIBBLE_MAX: u8 = 15;

/// Mask for one 4-bit nibble within a packed score/penalties byte.
pub const SCORE_NIBBLE_MASK: u8 = 0x0F;

const _: () = assert!(SCORE_NIBBLE_MAX == SCORE_NIBBLE_MASK);
