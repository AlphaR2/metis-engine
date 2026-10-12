//! A fixture's identity: a Merkle root over every provider's own ID for the match.
//!
//! The root is the oracle account's PDA seed, so "which match is this" is answered by hashes the
//! keepers can all recompute: each provider's ID is a leaf, and a verifier given one provider's
//! ID and the proof can check it belongs to this oracle. Leaves are sorted, so the order the IDs
//! are listed in doesn't matter.

use sha2::{Digest, Sha256};

/// The leaf for one provider's ID for this match, e.g. ("api-football", "1035037").
pub fn leaf(provider: &str, external_id: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    // 0x00 marks a leaf and 0x01 a node, so a leaf can never be mistaken for an inner hash.
    h.update([0x00]);
    h.update(b"metis:fixture:v1:");
    h.update(provider.as_bytes());
    h.update(b":");
    h.update(external_id.as_bytes());
    h.finalize().into()
}

fn node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update([0x01]);
    h.update(left);
    h.update(right);
    h.finalize().into()
}

/// The Merkle root over `ids` = (provider, external id) pairs. An odd leaf is carried up
/// unchanged rather than duplicated, so two different lists can't produce the same root.
pub fn fixture_root(ids: &[(&str, &str)]) -> [u8; 32] {
    assert!(!ids.is_empty(), "a fixture needs at least one provider id");
    let mut level: Vec<[u8; 32]> = ids.iter().map(|(p, e)| leaf(p, e)).collect();
    level.sort();
    level.dedup();
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|pair| if let [a, b] = pair { node(a, b) } else { pair[0] })
            .collect();
    }
    level[0]
}

pub fn to_hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_order_providers_are_listed_in_does_not_matter() {
        let a = fixture_root(&[("api-football", "1"), ("football-data", "2")]);
        let b = fixture_root(&[("football-data", "2"), ("api-football", "1")]);
        assert_eq!(a, b);
    }

    #[test]
    fn a_different_id_or_provider_gives_a_different_root() {
        let base = fixture_root(&[("api-football", "1"), ("football-data", "2")]);
        assert_ne!(base, fixture_root(&[("api-football", "1"), ("football-data", "3")]));
        assert_ne!(base, fixture_root(&[("api-football", "2"), ("football-data", "1")]));
        assert_ne!(base, fixture_root(&[("api-football", "1")]));
    }

    #[test]
    fn a_single_id_is_not_just_its_leaf() {
        // Hashing the one-leaf case through a node would be ambiguous; the root of one id is the
        // leaf itself, and that is distinct from any two-id root.
        assert_eq!(fixture_root(&[("a", "1")]), leaf("a", "1"));
    }

    #[test]
    fn hex_round_trips() {
        let root = fixture_root(&[("a", "1"), ("b", "2"), ("c", "3")]);
        assert_eq!(from_hex(&to_hex(&root)), Some(root));
        assert_eq!(from_hex("zz"), None);
    }
}
