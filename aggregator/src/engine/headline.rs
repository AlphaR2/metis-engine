//! Event headlines: what an event is about, as a readable title - never a question.
//!
//! Venues title events as questions ("Will China invade Taiwan by…?", "Which company has the best
//! AI model end of September?") - two thirds of Jupiter's do. An event groups questions, so its
//! title names the subject instead: "China to invade Taiwan", "Company with the best AI model end
//! of September". The questions themselves keep their full sentences.
//!
//! Rules, not a model: every title maps to the same headline on every build. A title no rule fits
//! keeps its own words, minus the question mark.

// The series-title helpers below are not used by the subject-based events (they title clusters from
// shared words instead); `headline` is. Kept for a venue event's own title.
#![allow(dead_code)]

use std::sync::LazyLock;

use regex::Regex;

use super::text;

const MONTH: &str = r"(?:jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\.?";

/// One date-ish phrase: "September 25, 2026", "Sep 18 - Sep 25", "2:30PM-2:35PM ET", "Q3", "2026".
static DATE_PHRASE: LazyLock<Regex> = LazyLock::new(|| {
    let day = format!(r"(?:{MONTH}\s+\d{{1,2}}(?:st|nd|rd|th)?|\d{{1,2}}(?:st|nd|rd|th)?\s+{MONTH})(?:,?\s+\d{{4}})?");
    let single = format!(r"(?:{day}|{MONTH}\s+\d{{4}}|\d{{4}}-\d{{2}}-\d{{2}}|\bQ[1-4]\b|\b{MONTH}\b)");
    let time = r"\d{1,2}(?::\d{2})?\s*(?:am|pm)(?:\s*[-–]\s*\d{1,2}(?::\d{2})?\s*(?:am|pm))?(?:\s*(?:et|pt|ct|utc|gmt))?";
    Regex::new(&format!(r"(?i)(?:{single}(?:\s*[-–]\s*{single})?(?:,?\s+\d{{4}})?|{time}|\b(?:19|20)\d{{2}}\b)")).unwrap()
});

/// Tokens for comparing titles: date phrases whole, then words, numbers and punctuation.
static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i){}|\$?\d[\d,.]*[kmbt%]?\+?|[\p{{L}}\p{{N}}'’$&.]+|_{{2,}}|\.\.\.|…|[^\s\p{{L}}\p{{N}}]", DATE_PHRASE.as_str().trim_start_matches("(?i)")))
        .unwrap()
});

static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$?\d[\d,.]*[kmbtKMBT%]?\+?").unwrap());
static BLANK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"_{2,}|\.\.\.|…").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// A title with its dates, times, years and numbers replaced by placeholders, lowercased - titles
/// with the same template are one series ("Bitcoin Up or Down - Sep 24, 3:30PM-3:35PM ET").
pub fn series_template(title: &str) -> String {
    let t = DATE_PHRASE.replace_all(title, " <d> ");
    let t = NUMBER.replace_all(&t, " <n> ");
    let t = BLANK.replace_all(&t, " <b> ");
    let t = t.to_lowercase().replace(['?', '.', ',', '(', ')', '-', '–', ':'], " ");
    SPACES.replace_all(t.trim(), " ").to_string()
}

/// The headline for a group of related titles: what they share, as a headline. Dates, numbers
/// and names that differ between them are dropped ("Will China invade Taiwan by Sep 30, 2026?" +
/// "…by Jun 30, 2027?" → "China to invade Taiwan").
pub fn common_headline(titles: &[&str]) -> String {
    let Some((first, rest)) = titles.split_first() else { return String::new() };
    let others: Vec<Vec<String>> = rest.iter().map(|t| tokens(t).into_iter().map(|(tok, _)| tok.to_lowercase()).collect()).collect();
    let kept: Vec<(String, bool)> =
        tokens(first).into_iter().filter(|(tok, _)| others.iter().all(|other| other.contains(&tok.to_lowercase()))).collect();
    headline(&tidy(&join(&kept)))
}

/// Tokens, each with whether whitespace preceded it in the title.
fn tokens(title: &str) -> Vec<(String, bool)> {
    TOKEN
        .find_iter(title)
        .map(|m| (m.as_str().to_string(), m.start() > 0 && title[..m.start()].ends_with(char::is_whitespace)))
        .collect()
}

/// Joins kept tokens back into text with the title's own spacing.
fn join(tokens: &[(String, bool)]) -> String {
    let mut out = String::new();
    for (tok, spaced) in tokens {
        if !out.is_empty() && *spaced {
            out.push(' ');
        }
        out.push_str(tok);
    }
    out
}

/// Cleans what dropping tokens leaves behind: dangling prepositions before the end, empty
/// brackets, a hyphen with nothing after it, doubled punctuation.
fn tidy(text: &str) -> String {
    static DANGLING: LazyLock<Regex> = LazyLock::new(|| {
        // Lowercase only: a capitalised "On" is a word of a name ("(Style Control On)").
        Regex::new(r"\s+(?:by|on|in|at|before|after|until|through|from|to|for|of|end of|week of|between|and|or|-|–|,)\s*([?:)]|$)").unwrap()
    });
    static EMPTY_BRACKETS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(\s*[-–,]?\s*\)").unwrap());
    static LOOSE_DASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*[-–]\s*([?:)]|$)|\s[-–]\s[-–]\s|([A-Za-z])-\s").unwrap());
    let mut t = text.to_string();
    for _ in 0..4 {
        let next = DANGLING.replace_all(&t, "$1").to_string();
        let next = EMPTY_BRACKETS.replace_all(&next, "").to_string();
        let next = LOOSE_DASH.replace_all(&next, |c: &regex::Captures| match (c.get(1), c.get(2)) {
            (Some(end), _) => end.as_str().to_string(),
            (None, Some(letter)) => format!("{} ", letter.as_str()),
            _ => " ".to_string(),
        });
        let next = SPACES.replace_all(next.trim(), " ").replace(" ?", "?").replace(" ,", ",").replace(",?", "?");
        if next == t {
            break;
        }
        t = next;
    }
    t
}

/// A venue's event title as a headline.
pub fn headline(title: &str) -> String {
    let t = SPACES.replace_all(title.trim(), " ").to_string();
    let t = drop_blanks(&t);
    let t = t.trim().trim_end_matches(['?', '.', ' ']).to_string();
    // A trailing bracket ("(Week 11)", "(Style Control On)") is kept aside and put back.
    let (body, bracket) = split_trailing_bracket(&t);
    let body = body.trim_end_matches(['?', ' ']).trim();
    let words: Vec<&str> = body.split(' ').collect();
    let first = text::fold(words.first().copied().unwrap_or(""));

    let rewritten = match first.as_str() {
        "will" => will_headline(&words[1..]),
        "who" | "what" | "which" | "when" | "where" | "how" => wh_headline(&words),
        "is" | "are" if words.len() > 2 => {
            // "Is Earth flat" → "Earth is flat": the verb goes after the leading name.
            let name = words[1..].iter().take_while(|w| w.starts_with(char::is_uppercase)).count().max(1);
            format!("{} {} {}", words[1..=name].join(" "), first, words[name + 1..].join(" "))
        }
        "does" | "do" | "did" | "can" | "has" | "have" if words.len() > 2 => words[1..].join(" "),
        _ => statement_headline(body),
    };
    let mut out = capitalize(rewritten.trim());
    if let Some(bracket) = bracket {
        out = format!("{out} {bracket}");
    }
    out
}

/// "above ___", "hit __", "by...?" - a blank the venue fills per market - read as nothing.
fn drop_blanks(title: &str) -> String {
    static MILESTONE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:will\s+)?(.+?)\s+(?:hit|reach|surpass|exceed|be above|be over)\s*(?:_{2,}|\.\.\.|…)\s*").unwrap()
    });
    static BLANK_PHRASE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\s*(?:\b(?:by|on|before|after|in|at|above|below|over|under|at least|more than|less than|between)\s*)?(?:_{2,}|\.\.\.|…)").unwrap()
    });
    static FLOOR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:will\s+)?(.+?)\s+(?:fall|drop|dip|sink)\s+(?:to|below|under)\s*(?:_{2,}|\.\.\.|…)\s*").unwrap()
    });
    // "Will OpenAI's valuation hit __ by December 31?" → "OpenAI's valuation milestones by December 31"
    if let Some(c) = MILESTONE.captures(title) {
        let rest = &title[c.get(0).unwrap().end()..];
        return format!("{} milestones {rest}", &c[1]);
    }
    // "Will US crude oil reserves fall to __ by October 30?" → "US crude oil reserves lows by October 30"
    if let Some(c) = FLOOR.captures(title) {
        let rest = &title[c.get(0).unwrap().end()..];
        return format!("{} lows {rest}", &c[1]);
    }
    BLANK_PHRASE.replace_all(title, "").to_string()
}

fn split_trailing_bracket(text: &str) -> (String, Option<String>) {
    if text.ends_with(')')
        && let Some(open) = text.rfind('(')
        && open > 0
    {
        return (text[..open].trim().to_string(), Some(text[open..].to_string()));
    }
    (text.to_string(), None)
}

/// "Will Argentina dollarize by June 30, 2026" → "Argentina to dollarize by June 30, 2026".
fn will_headline(rest: &[&str]) -> String {
    if rest.first().is_some_and(|w| text::fold(w) == "there") && rest.get(1).is_some_and(|w| text::fold(w) == "be") {
        return strip_article(&rest[2..].join(" "));
    }
    match verb_index(rest, 1) {
        Some(v) => format!("{} to {}", rest[..v].join(" "), rest[v..].join(" ")),
        None => rest.join(" "),
    }
}

/// Wh-questions become noun phrases: "Which airlines will announce bankruptcy" → "Airlines
/// announcing bankruptcy"; "How many Gold Cards will Trump sell" → "Number of Gold Cards Trump will
/// sell"; "Who will be the next Secretary-General" → "Next Secretary-General".
fn wh_headline(words: &[&str]) -> String {
    let folded: Vec<String> = words.iter().map(|w| text::fold(w)).collect();
    let find = |word: &str| folded.iter().position(|w| w == word);
    let lead = folded[0].as_str();

    // "How many N …" / "How much N …" → "Number of N …" / "Amount of N …"
    let (noun_phrase, after): (Vec<&str>, usize) = match (lead, folded.get(1).map(String::as_str)) {
        ("how", Some("many")) => {
            let end = find("will").unwrap_or(words.len());
            (std::iter::once("Number of").chain(words[2..end].iter().copied()).collect(), end)
        }
        ("how", Some("much")) => {
            let end = find("will").unwrap_or(words.len());
            (std::iter::once("Amount of").chain(words[2..end].iter().copied()).collect(), end)
        }
        ("how", _) => (Vec::new(), 0),
        ("what" | "which", _) => {
            // "What price will X hit" / "Which company has": the noun right after the wh-word.
            let end = folded.iter().position(|w| ["will", "has", "have", "is", "are", "was", "does", "do"].contains(&w.as_str())).unwrap_or(1);
            (words[1..end].to_vec(), end)
        }
        _ => (Vec::new(), 1),
    };

    if lead == "how" && noun_phrase.is_empty() {
        // "How high will Bitcoin go in 2026" → "How high Bitcoin will go in 2026"
        if let Some(w) = find("will") {
            let rest = &words[w + 1..];
            return match verb_index(rest, 1) {
                Some(v) => format!("{} {} will {}", words[..w].join(" "), rest[..v].join(" "), rest[v..].join(" ")),
                None => words.join(" "),
            };
        }
        return words.join(" ");
    }

    let aux = folded.get(after).map(String::as_str);
    let rest = if after < words.len() { &words[after + 1..] } else { &[][..] };
    let subject_noun = if noun_phrase.is_empty() {
        match lead {
            "who" => "People".to_string(),
            "what" => String::new(),
            "when" => String::new(),
            "where" => String::new(),
            _ => String::new(),
        }
    } else {
        noun_phrase.join(" ")
    };

    match aux {
        Some("will") => {
            // "Who will be X" / "What will be X" → "X"; "Who will be arrested" → "People arrested";
            // "What will be in X" → "What's in X".
            if rest.first().is_some_and(|w| text::fold(w) == "be") && matches!(lead, "who" | "what") && noun_phrase.is_empty() {
                let x = &rest[1..];
                let next = x.first().map(|w| text::fold(w)).unwrap_or_default();
                if PREPOSITIONS.contains(&next.as_str()) {
                    return format!("What's {}", x.join(" "));
                }
                if lead == "who" && next.len() > 3 && next.ends_with("ed") {
                    return format!("People {}", x.join(" "));
                }
                return strip_article(&x.join(" "));
            }
            // "What price will X hit Y" / "What will X hit Y" → "X price Y"
            if matches!(lead, "what") && (noun_phrase.is_empty() || noun_phrase.iter().any(|w| text::fold(w) == "price"))
                && let Some(v) = verb_index(rest, 1).filter(|&v| ["hit", "reach", "close", "trade", "be"].contains(&text::fold(rest[v]).as_str()))
            {
                let tail = rest[v + 1..].join(" ");
                return format!("{} price {tail}", rest[..v].join(" "));
            }
            // "Who will win the X" → "X winner"
            if lead == "who" && rest.first().is_some_and(|w| text::fold(w) == "win") {
                return format!("{} winner", strip_article(&rest[1..].join(" ")));
            }
            match verb_index(rest, 0) {
                // The wh-phrase is the subject: "Which airlines will announce …" → "Airlines announcing …"
                Some(0) if !subject_noun.is_empty() => format!("{subject_noun} {} {}", gerund(rest[0]), rest[1..].join(" ")),
                // Someone else is: "How many Gold Cards will Trump sell …" → "… Trump will sell …"
                Some(v) if v > 0 => {
                    let prefix = if subject_noun.is_empty() { capitalize(lead) } else { subject_noun };
                    format!("{prefix} {} will {}", rest[..v].join(" "), rest[v..].join(" "))
                }
                _ => words.join(" "),
            }
        }
        // "Which company has the best AI model" → "Company with the best AI model"
        Some("has" | "have") if !subject_noun.is_empty() => format!("{subject_noun} with {}", rest.join(" ")),
        Some("is" | "are") if !subject_noun.is_empty() => format!("{subject_noun} that {} {}", aux.unwrap(), rest.join(" ")),
        _ if !subject_noun.is_empty() && lead == "how" => format!("{subject_noun} {}", rest.join(" ")).trim().to_string(),
        _ => words.join(" "),
    }
}

/// A title that is already a statement, with a few fix-ups: "# of views" → "Number of views",
/// "Elon Musk # tweets" → "Elon Musk tweet count".
fn statement_headline(body: &str) -> String {
    static HASH_COUNT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(.+?)\s+#\s+(\p{L}+?)s?\b(.*)$").unwrap());
    if let Some(rest) = body.strip_prefix("# of ") {
        return format!("Number of {rest}");
    }
    if let Some(c) = HASH_COUNT.captures(body) {
        return format!("{} {} count{}", &c[1], &c[2], &c[3]);
    }
    if let Some(rest) = body.strip_prefix("# ").or_else(|| body.strip_prefix('#')) {
        return format!("{} rankings", rest.trim());
    }
    body.to_string()
}

fn strip_article(text: &str) -> String {
    for article in ["the ", "a ", "an "] {
        if text.len() > article.len() && text[..article.len()].eq_ignore_ascii_case(article) {
            return text[article.len()..].to_string();
        }
    }
    text.to_string()
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Where the verb is in "Argentina dollarize by …", searching from `from`: the first word that is
/// a known verb, or reads as one (`-ize`, `-ify`). Lowercase only - a capitalised word is a name.
fn verb_index(words: &[&str], from: usize) -> Option<usize> {
    words.iter().enumerate().skip(from).find(|(_, w)| is_verb(w)).map(|(i, _)| i)
}

fn is_verb(word: &str) -> bool {
    if word.chars().next().is_none_or(|c| !c.is_lowercase()) {
        return false;
    }
    let w = word.trim_matches(|c: char| !c.is_alphabetic());
    VERBS.contains(&w) || ((w.ends_with("ize") || w.ends_with("ise") || w.ends_with("ify")) && w.len() > 5)
}

/// "announce" → "announcing", "win" → "winning", "vote" → "voting".
fn gerund(verb: &str) -> String {
    let v = verb.to_lowercase();
    if v == "be" {
        return "being".into();
    }
    if let Some(stem) = v.strip_suffix("ie") {
        return format!("{stem}ying");
    }
    if v.ends_with('e') && !v.ends_with("ee") && !v.ends_with("ye") && !v.ends_with("oe") {
        return format!("{}ing", &v[..v.len() - 1]);
    }
    let chars: Vec<char> = v.chars().collect();
    let vowel = |c: char| "aeiou".contains(c);
    let vowel_groups = chars.windows(2).filter(|w| !vowel(w[0]) && vowel(w[1])).count() + usize::from(chars.first().is_some_and(|c| vowel(*c)));
    if chars.len() >= 3 && vowel_groups == 1 {
        let (a, b, c) = (chars[chars.len() - 3], chars[chars.len() - 2], chars[chars.len() - 1]);
        if !vowel(a) && vowel(b) && !vowel(c) && !"wxy".contains(c) {
            return format!("{v}{c}ing");
        }
    }
    format!("{v}ing")
}

const PREPOSITIONS: &[&str] = &["in", "on", "at", "by", "for", "from", "with", "about", "inside", "among"];

/// Base-form verbs that turn up in market titles.
const VERBS: &[&str] = &[
    "accept", "acquire", "add", "advance", "agree", "air", "announce", "appear", "appoint", "approve", "arrest", "attack", "attend",
    "ban", "be", "beat", "become", "begin", "break", "buy", "call", "capture", "change", "charge", "claim", "clear", "close", "come",
    "complete", "confirm", "convict", "cross", "cut", "debut", "decide", "declare", "default", "defeat", "delay", "deliver", "deploy",
    "die", "drop", "earn", "end", "endorse", "enter", "exceed", "exit", "face", "fall", "file", "finish", "fire", "form", "freeze",
    "gain", "get", "give", "go", "grow", "happen", "have", "hike", "hit", "hold", "host", "impose", "increase", "introduce", "invade",
    "issue", "join", "launch", "lead", "leave", "list", "lose", "make", "meet", "merge", "miss", "move", "name", "nominate", "open",
    "order", "outperform", "overtake", "pardon", "pass", "pay", "perform", "play", "post", "publish", "qualify", "raise", "rank",
    "reach", "receive", "recognize", "record", "reduce", "reject", "release", "remain", "remove", "replace", "report", "resign",
    "resume", "retire", "return", "reveal", "rise", "rule", "run", "say", "score", "secure", "see", "sell", "send", "sentence", "serve",
    "set", "settle", "ship", "sign", "speak", "split", "stake", "start", "stay", "step", "strike", "submit", "succeed", "sue", "surpass",
    "suspend", "take", "top", "trade", "transfer", "tweet", "unveil", "visit", "vote", "walk", "win", "withdraw",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn will_questions_become_to_headlines() {
        assert_eq!(headline("Will China invade Taiwan by December 31, 2027?"), "China to invade Taiwan by December 31, 2027");
        assert_eq!(headline("Will Argentina dollarize by June 30, 2026?"), "Argentina to dollarize by June 30, 2026");
        assert_eq!(headline("Will Vest launch a token by ___?"), "Vest to launch a token");
        assert_eq!(headline("Will Russia capture Sumy by...?"), "Russia to capture Sumy");
        assert_eq!(headline("Will Nike (NKE) beat quarterly earnings?"), "Nike (NKE) to beat quarterly earnings");
        assert_eq!(headline("Will OpenAI's valuation hit __ by December 31?"), "OpenAI's valuation milestones by December 31");
    }

    #[test]
    fn wh_questions_become_noun_phrases() {
        assert_eq!(headline("Which airlines will announce bankruptcy by December 31?"), "Airlines announcing bankruptcy by December 31");
        assert_eq!(headline("Which Senators will vote for the Clarity Act?"), "Senators voting for the Clarity Act");
        assert_eq!(headline("Which company has the best AI model end of September?"), "Company with the best AI model end of September");
        assert_eq!(headline("How many Gold Cards will Trump sell in 2026?"), "Number of Gold Cards Trump will sell in 2026");
        assert_eq!(headline("How many albums will reach Billboard #1 in 2026?"), "Number of albums reaching Billboard #1 in 2026");
        assert_eq!(headline("Who will be the next Secretary-General of the UN?"), "Next Secretary-General of the UN");
        assert_eq!(headline("What will be the top US Netflix movie this week?"), "Top US Netflix movie this week");
        assert_eq!(headline("What price will Solana hit September 21-27?"), "Solana price September 21-27");
        assert_eq!(headline("When will Bitcoin hit $100k?"), "When Bitcoin will hit $100k");
        assert_eq!(headline("Who will be evicted from Big Brother? (Week 11)"), "People evicted from Big Brother (Week 11)");
        assert_eq!(headline("What will be in a US-Iran deal in 2026?"), "What's in a US-Iran deal in 2026");
        assert_eq!(headline("Is Earth flat?"), "Earth is flat");
        assert_eq!(headline("Will US crude oil reserves fall to __ by October 30?"), "US crude oil reserves lows by October 30");
    }

    #[test]
    fn statements_lose_only_the_question_mark() {
        assert_eq!(headline("Hantavirus pandemic in 2026?"), "Hantavirus pandemic in 2026");
        assert_eq!(headline("Lisa Cook officially out as Fed Governor by...?"), "Lisa Cook officially out as Fed Governor");
        assert_eq!(headline("# of views of next MrBeast Gaming video on day 1?"), "Number of views of next MrBeast Gaming video on day 1");
        assert_eq!(headline("NFL Champion 2027"), "NFL Champion 2027");
        assert_eq!(headline("Levante UD vs. Athletic Club"), "Levante UD vs. Athletic Club");
    }

    #[test]
    fn a_series_shares_a_template_and_a_headline() {
        let titles = ["Will China invade Taiwan by September 30, 2026?", "Will China invade Taiwan by June 30, 2027?", "Will China invade Taiwan by December 31, 2027?"];
        assert_eq!(series_template(titles[0]), series_template(titles[2]));
        assert_eq!(common_headline(&titles), "China to invade Taiwan");

        let tweets = ["Elon Musk # tweets September 18 - September 25, 2026?", "Elon Musk # tweets September 25 - October 2, 2026?"];
        assert_eq!(series_template(tweets[0]), series_template(tweets[1]));
        assert_eq!(common_headline(&tweets), "Elon Musk tweet count");

        let races = ["CA-28 House Election Winner", "CA-15 House Election Winner"];
        assert_eq!(series_template(races[0]), series_template(races[1]));
        assert_eq!(common_headline(&races), "CA House Election Winner");
    }

    #[test]
    fn different_subjects_are_different_series() {
        assert_ne!(series_template("Bitcoin above ___ on September 25?"), series_template("Ethereum above ___ on September 25?"));
        assert_ne!(series_template("NFL Champion 2027"), series_template("NBA Champion 2027"));
    }

    #[test]
    fn gerunds() {
        assert_eq!(gerund("win"), "winning");
        assert_eq!(gerund("vote"), "voting");
        assert_eq!(gerund("announce"), "announcing");
        assert_eq!(gerund("release"), "releasing");
        assert_eq!(gerund("reach"), "reaching");
        assert_eq!(gerund("die"), "dying");
    }
}
