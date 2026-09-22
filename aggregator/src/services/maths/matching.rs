//! Pure string-similarity math backing event matching. No graph access - just two titles in, a
//! yes/no answer out. `services::matching::service` owns the graph-querying orchestration.

/// Minimum title similarity (0.0-1.0, from `strsim::normalized_levenshtein`) to treat two markets
/// as the same event. Chosen conservatively: a false match silently merges two different events,
/// which is worse than a false split (which just costs a `needs_confirmation` flag for a human to
/// resolve).
const TITLE_SIMILARITY_THRESHOLD: f64 = 0.72;

/// Lowercases and strips punctuation so similarity ignores casing/formatting noise.
fn normalize_title(title: &str) -> String {
    title.to_lowercase().chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect()
}

/// True if two titles are similar enough to be the same real-world event. Levenshtein-based -
/// handles minor per-venue formatting differences (case, punctuation, a trailing qualifier), but
/// deliberately does NOT resolve genuine abbreviations like "Man Utd" vs "Manchester United"
/// (that needs a team-name dictionary, out of scope for this "basic" matcher).
pub fn titles_match(a: &str, b: &str) -> bool {
    let (a, b) = (normalize_title(a), normalize_title(b));
    strsim::normalized_levenshtein(&a, &b) >= TITLE_SIMILARITY_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_titles_match() {
        assert!(titles_match("Man Utd vs Fulham", "Man Utd vs Fulham"));
    }

    #[test]
    fn case_and_punctuation_differences_still_match() {
        assert!(titles_match("Man Utd vs Fulham", "man utd vs fulham"));
    }

    #[test]
    fn genuine_abbreviations_do_not_match() {
        assert!(!titles_match("Man Utd vs Fulham", "Manchester United vs Fulham"));
    }

    #[test]
    fn unrelated_titles_do_not_match() {
        assert!(!titles_match("Man Utd vs Fulham", "Lakers vs Celtics"));
    }
}
