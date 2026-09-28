//! Text normalisation for extraction and matching: case and accent folding, tokens, numbers
//! with magnitude suffixes, and dates. Pure functions, no venue knowledge.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use chrono::{Datelike, NaiveDate};
use regex::Regex;
use rust_decimal::Decimal;

/// Folds the accented Latin letters venues actually use ("Flávio", "Côte d'Ivoire", "Édouard")
/// to ASCII, so spellings with and without accents compare equal.
fn fold_char(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' => 'a',
        'é' | 'è' | 'ê' | 'ë' | 'ē' | 'ę' => 'e',
        'í' | 'ì' | 'î' | 'ï' | 'ī' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' | 'ō' => 'o',
        'ú' | 'ù' | 'û' | 'ü' | 'ū' => 'u',
        'ñ' | 'ń' => 'n',
        'ç' | 'ć' | 'č' => 'c',
        'ş' | 'š' | 'ś' => 's',
        'ž' | 'ź' | 'ż' => 'z',
        'ł' => 'l',
        'ğ' => 'g',
        'ý' | 'ÿ' => 'y',
        other => other,
    }
}

/// Lowercased, accent-folded, with curly quotes and dashes made plain.
pub fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(fold_char)
        .map(|c| match c {
            '’' | '‘' | '`' => '\'',
            '“' | '”' => '"',
            '–' | '—' | '−' => '-',
            other => other,
        })
        .collect()
}

/// A stable identifier fragment: folded, with every run of non-alphanumerics turned into `-`.
pub fn slug(text: &str) -> String {
    let folded = fold(text);
    let mut out = String::with_capacity(folded.len());
    for c in folded.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Words that carry no identifying meaning in a market title.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "of", "in", "on", "at", "to", "by", "for", "and", "or", "be", "is", "will", "vs", "v", "with",
    "their", "its", "this", "that", "than", "from", "as", "any", "point", "during", "between", "who", "what", "which",
    "how", "many", "much", "win", "wins", "end", "yes", "no", "market", "price", "priced", "or", "higher", "lower",
];

/// The distinct meaningful words of `text`: folded, stopwords and bare numbers removed.
pub fn terms(text: &str) -> BTreeSet<String> {
    fold(text)
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() > 1 && !STOPWORDS.contains(w) && !w.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_string)
        .collect()
}

static NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    // An optional currency sign, digits with thousands separators and decimals, then an optional
    // magnitude suffix. The suffix must not run into a following word ("5 billion" is handled by
    // WORD_MAGNITUDE; "5bn" and "2.25T" here).
    Regex::new(r"(?i)\$?\s?(\d{1,3}(?:,\d{3})+|\d+)(\.\d+)?\s?(k|m|bn|b|t)?\b").unwrap()
});

static WORD_MAGNITUDE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(thousand|million|billion|trillion)\b").unwrap());

/// Every number in `text`, with `k`/`m`/`b`/`t` (and the spelled-out words) applied:
/// "$81,000.00" → 81000, "100k" → 100000, "$2.25T" → 2250000000000, "5 billion" → 5000000000.
/// In a range written with one suffix ("$2.0–$2.25T", "500–750B") the suffix applies to both ends.
/// Years and small counts come through as plain numbers; callers decide what they mean.
pub fn numbers(text: &str) -> Vec<Decimal> {
    static RANGE_JOIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*(?:-|–|—|to)\s*$").unwrap());

    // (start, end, value before magnitude, magnitude if written)
    let mut raw: Vec<(usize, usize, Decimal, Option<i64>)> = Vec::new();
    for caps in NUMBER.captures_iter(text) {
        let whole = caps.get(1).map_or("", |m| m.as_str()).replace(',', "");
        let frac = caps.get(2).map_or("", |m| m.as_str());
        let Ok(value) = format!("{whole}{frac}").parse::<Decimal>() else { continue };
        let span = caps.get(0).unwrap();
        let suffix = caps.get(3).map(|m| m.as_str().to_lowercase());
        let word = WORD_MAGNITUDE.captures(&text[span.end()..]).map(|c| c[1].to_lowercase());
        let multiplier = match suffix.as_deref().or(word.as_deref()) {
            Some("k") | Some("thousand") => Some(1_000),
            Some("m") | Some("million") => Some(1_000_000),
            Some("b") | Some("bn") | Some("billion") => Some(1_000_000_000),
            Some("t") | Some("trillion") => Some(1_000_000_000_000),
            _ => None,
        };
        raw.push((span.start(), span.end(), value, multiplier));
    }
    (0..raw.len())
        .map(|i| {
            let (_, end, value, multiplier) = raw[i];
            let multiplier = multiplier.or_else(|| {
                let (next_start, _, _, next_multiplier) = *raw.get(i + 1)?;
                let between = text.get(end..next_start)?.trim_end_matches('$');
                RANGE_JOIN.is_match(between).then_some(next_multiplier).flatten()
            });
            (value * Decimal::from(multiplier.unwrap_or(1))).normalize()
        })
        .collect()
}

/// A date as written: the year may be missing ("September 21").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PartialDate {
    pub year: Option<i32>,
    pub month: u32,
    pub day: u32,
}

impl PartialDate {
    /// Fills a missing year from `reference` (typically the market's close date): the first
    /// occurrence of this month/day on or after `reference`, less a week of slack.
    pub fn resolve(self, reference: NaiveDate) -> Option<NaiveDate> {
        if let Some(year) = self.year {
            return NaiveDate::from_ymd_opt(year, self.month, self.day);
        }
        let floor = reference - chrono::Duration::days(7);
        [floor.year(), floor.year() + 1]
            .into_iter()
            .filter_map(|y| NaiveDate::from_ymd_opt(y, self.month, self.day))
            .find(|d| *d >= floor)
    }
}

const MONTHS: &[(&str, u32)] = &[
    ("january", 1), ("jan", 1), ("february", 2), ("feb", 2), ("march", 3), ("mar", 3), ("april", 4), ("apr", 4),
    ("may", 5), ("june", 6), ("jun", 6), ("july", 7), ("jul", 7), ("august", 8), ("aug", 8), ("september", 9),
    ("sept", 9), ("sep", 9), ("october", 10), ("oct", 10), ("november", 11), ("nov", 11), ("december", 12), ("dec", 12),
];

static MONTH_DAY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(january|february|march|april|may|june|july|august|september|october|november|december|jan|feb|mar|apr|jun|jul|aug|sept|sep|oct|nov|dec)\.?\s+(\d{1,2})(?:st|nd|rd|th)?\b(?:,?\s+(\d{4}))?").unwrap()
});
static DAY_MONTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(\d{1,2})(?:st|nd|rd|th)?\s+(january|february|march|april|may|june|july|august|september|october|november|december|jan|feb|mar|apr|jun|jul|aug|sept|sep|oct|nov|dec)\b(?:,?\s+(\d{4}))?").unwrap()
});
static ISO_DATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\d{4})-(\d{2})-(\d{2})\b").unwrap());
static YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(20\d{2})\b").unwrap());

fn month_number(name: &str) -> Option<u32> {
    let name = name.to_lowercase();
    MONTHS.iter().find(|(m, _)| *m == name).map(|(_, n)| *n)
}

/// Every calendar date written in `text`: "September 8, 2026", "Sep 25", "1st August 2026",
/// "2026-09-25".
pub fn dates(text: &str) -> Vec<PartialDate> {
    let mut out = Vec::new();
    for caps in MONTH_DAY.captures_iter(text) {
        let (Some(month), Ok(day)) = (month_number(&caps[1]), caps[2].parse::<u32>()) else { continue };
        let year = caps.get(3).and_then(|y| y.as_str().parse().ok());
        if (1..=31).contains(&day) {
            out.push(PartialDate { year, month, day });
        }
    }
    for caps in DAY_MONTH.captures_iter(text) {
        let (Ok(day), Some(month)) = (caps[1].parse::<u32>(), month_number(&caps[2])) else { continue };
        let year = caps.get(3).and_then(|y| y.as_str().parse().ok());
        let date = PartialDate { year, month, day };
        if (1..=31).contains(&day) && !out.contains(&date) {
            out.push(date);
        }
    }
    for caps in ISO_DATE.captures_iter(text) {
        if let (Ok(year), Ok(month), Ok(day)) = (caps[1].parse(), caps[2].parse(), caps[3].parse()) {
            out.push(PartialDate { year: Some(year), month, day });
        }
    }
    out
}

/// Four-digit years 2000–2099 mentioned in `text`.
pub fn years(text: &str) -> Vec<i32> {
    YEAR.captures_iter(text).filter_map(|c| c[1].parse().ok()).collect()
}

/// UTC calendar date of a Unix timestamp; `None` for 0 ("unknown").
pub fn date_of(unix: i64) -> Option<NaiveDate> {
    (unix > 0).then(|| chrono::DateTime::from_timestamp(unix, 0).map(|dt| dt.date_naive())).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn folding_ignores_case_accents_and_typographic_quotes() {
        assert_eq!(fold("Côte d’Ivoire"), "cote d'ivoire");
        assert_eq!(slug("Flávio Bolsonaro"), "flavio-bolsonaro");
        assert_eq!(slug("  Atlanta Dream — WNBA  "), "atlanta-dream-wnba");
    }

    #[test]
    fn numbers_apply_separators_and_magnitudes() {
        assert_eq!(numbers("Will Bitcoin be priced at $81,000.00 or higher"), vec![d("81000")]);
        assert_eq!(numbers("↑ 90,000"), vec![d("90000")]);
        assert_eq!(numbers("$2.0–$2.25T"), vec![d("2000000000000"), d("2250000000000")]);
        assert_eq!(numbers("500–750B"), vec![d("500000000000"), d("750000000000")]);
        assert_eq!(numbers("at least 206,000 claims, 100k views, 5 billion"), vec![d("206000"), d("100000"), d("5000000000")]);
        assert_eq!(numbers("15+"), vec![d("15")]);
    }

    #[test]
    fn dates_come_in_several_shapes() {
        let sept8 = PartialDate { year: Some(2026), month: 9, day: 8 };
        assert_eq!(dates("on Tuesday, September 8, 2026, at 12:00 PM"), vec![sept8]);
        assert_eq!(dates("by 9.30 am CT, August 1st 2026"), vec![PartialDate { year: Some(2026), month: 8, day: 1 }]);
        assert_eq!(dates("September 21"), vec![PartialDate { year: None, month: 9, day: 21 }]);
        assert_eq!(dates("closes 2026-09-25"), vec![PartialDate { year: Some(2026), month: 9, day: 25 }]);
        assert_eq!(dates("on 1st August 2026"), vec![PartialDate { year: Some(2026), month: 8, day: 1 }]);
    }

    #[test]
    fn a_yearless_date_resolves_against_the_reference_date() {
        let dec31 = PartialDate { year: None, month: 12, day: 31 };
        let reference = NaiveDate::from_ymd_opt(2026, 9, 24).unwrap();
        assert_eq!(dec31.resolve(reference), NaiveDate::from_ymd_opt(2026, 12, 31));
        let jan5 = PartialDate { year: None, month: 1, day: 5 };
        assert_eq!(jan5.resolve(reference), NaiveDate::from_ymd_opt(2027, 1, 5));
    }

    #[test]
    fn terms_drop_stopwords_and_bare_numbers() {
        let t = terms("Will the Norway vs. Côte d'Ivoire match be decided by a penalty shootout in 2026?");
        assert!(t.contains("norway") && t.contains("cote") && t.contains("shootout"));
        assert!(!t.contains("the") && !t.contains("vs") && !t.contains("2026"));
    }
}
