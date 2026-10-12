//! A market's human-readable side: its question, rubrics and labels.
//!
//! Kept in its own account, written once by a separate instruction, because a Solana
//! transaction is capped at 1,232 bytes and the text doesn't fit alongside everything
//! `initialize_market` already needs. The account is a PDA that can only be created, never
//! rewritten, so what a bettor read when they bet is what settlement is judged against.

use bytemuck::{Pod, Zeroable};
use pinocchio::Address;

pub const QUESTION_MAX: usize = 160;
pub const RUBRIC_MAX: usize = 240;
pub const LABEL_MAX: usize = 24;
pub const CATEGORY_MAX: usize = 24;
pub const LEAGUE_MAX: usize = 48;

/// Account discriminator: sha256("account:MarketMetadata")[..8], see tools/compute-discriminators.
pub const MARKET_METADATA_DISCRIMINATOR: [u8; 8] = [12, 16, 109, 58, 31, 252, 133, 8];

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct MarketMetadata {
    pub discriminator: [u8; 8],
    pub market: Address,
    pub question: [u8; QUESTION_MAX],
    /// What has to be true for YES to win, in words. Must agree with the machine-checkable
    /// condition on the market; a UI shows both.
    pub rubric_yes: [u8; RUBRIC_MAX],
    pub rubric_no: [u8; RUBRIC_MAX],
    pub label_yes: [u8; LABEL_MAX],
    pub label_no: [u8; LABEL_MAX],
    pub category: [u8; CATEGORY_MAX],
    pub league: [u8; LEAGUE_MAX],
    /// Used byte counts: question, rubric_yes, rubric_no, label_yes, label_no, category, league.
    pub lens: [u16; 7],
}

impl MarketMetadata {
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// `field(i)` indexes: 0 question, 1 rubric_yes, 2 rubric_no, 3 label_yes, 4 label_no,
    /// 5 category, 6 league. Returns the used bytes, or `None` if the stored length is nonsense.
    pub fn field(&self, i: usize) -> Option<&[u8]> {
        let buf: &[u8] = match i {
            0 => &self.question,
            1 => &self.rubric_yes,
            2 => &self.rubric_no,
            3 => &self.label_yes,
            4 => &self.label_no,
            5 => &self.category,
            6 => &self.league,
            _ => return None,
        };
        buf.get(..*self.lens.get(i)? as usize)
    }
}

/// Whether a field is acceptable: non-empty, within `max`, and valid UTF-8 with no control
/// characters (a market's question must render as text, not escape sequences).
pub fn valid_text(bytes: &[u8], max: usize) -> bool {
    !bytes.is_empty()
        && bytes.len() <= max
        && core::str::from_utf8(bytes).is_ok_and(|s| !s.chars().any(char::is_control))
}

const _: () = assert!(MarketMetadata::LEN == 8 + 32 + 160 + 240 + 240 + 24 + 24 + 24 + 48 + 14);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_rules() {
        assert!(valid_text("Will Arsenal win?".as_bytes(), QUESTION_MAX));
        assert!(valid_text("日本が勝つ".as_bytes(), LABEL_MAX), "non-ASCII is fine");
        assert!(!valid_text(b"", QUESTION_MAX), "empty");
        assert!(!valid_text(&[b'a'; QUESTION_MAX + 1], QUESTION_MAX), "too long");
        assert!(!valid_text(b"line\nbreak", QUESTION_MAX), "control character");
        assert!(!valid_text(&[0xff, 0xfe], QUESTION_MAX), "not utf-8");
    }

    #[test]
    fn fields_come_back_at_their_stored_lengths() {
        let mut m = MarketMetadata::zeroed();
        m.question[..5].copy_from_slice(b"hello");
        m.lens[0] = 5;
        assert_eq!(m.field(0), Some(&b"hello"[..]));
        assert_eq!(m.field(1), Some(&b""[..]));
        assert_eq!(m.field(7), None);
        m.lens[0] = (QUESTION_MAX + 1) as u16;
        assert_eq!(m.field(0), None, "a corrupt length is not trusted");
    }
}
