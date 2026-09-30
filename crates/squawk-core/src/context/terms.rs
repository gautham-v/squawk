//! Jargon from a repo's docs (README.md, CLAUDE.md, AGENTS.md, CONTEXT.md).
//!
//! Prose is full of capitalised words that are not names — every sentence
//! starts with one, and headings are title case — so a term has to earn its
//! place:
//! - inner capitals with some lowercase (`ClaudeBar`, `macOS`, `GitHub`), or a
//!   letter/digit mix (`gpt5`, `M4`), anywhere outside code blocks;
//! - an acronym (`GPUI`, `TUI`) that the docs never write in lowercase;
//! - a Capitalised word (`Tidewell`, `Tokio`) seen mid-sentence at least
//!   once and never written in lowercase;
//! - a kebab/camel identifier in `inline code`.
//!
//! Stopwords never count ("Settings" in "open Settings" stays out).

use std::collections::HashMap;

use super::stopwords::is_stopword;
use crate::text::has_inner_case;

/// Longest acronym worth keeping: past this it is shouting ("IMPORTANT").
const MAX_ACRONYM: usize = 8;

#[derive(Default)]
struct Stat {
    order: usize,
    /// Spellings that would qualify, with counts.
    spellings: Vec<(String, usize)>,
    /// Always qualifies (camel, digit mix, code identifier).
    strong: bool,
    acronym: bool,
    capitalised_mid: bool,
    lowercase: bool,
}

impl Stat {
    fn spelled(&mut self, word: &str) {
        match self.spellings.iter_mut().find(|(s, _)| s == word) {
            Some((_, n)) => *n += 1,
            None => self.spellings.push((word.to_string(), 1)),
        }
    }

    fn best(&self) -> Option<&str> {
        // Most frequent spelling; the first seen on a tie.
        let mut best: Option<&(String, usize)> = None;
        for s in &self.spellings {
            if best.is_none_or(|b| s.1 > b.1) {
                best = Some(s);
            }
        }
        best.map(|(s, _)| s.as_str())
    }

    fn qualifies(&self) -> bool {
        self.strong || (!self.lowercase && (self.acronym || self.capitalised_mid))
    }
}

/// Terms in canonical spelling, in order of first appearance.
pub(crate) fn extract(docs: &str) -> Vec<String> {
    let mut stats: HashMap<String, Stat> = HashMap::new();
    let mut in_fence = false;
    for line in docs.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let heading = trimmed.starts_with('#');
        // Odd pieces between backticks are inline code.
        let mut sentence_start = true;
        for (n, piece) in trimmed.split('`').enumerate() {
            if n % 2 == 1 {
                code_term(piece, &mut stats);
                sentence_start = false;
                continue;
            }
            for raw in piece.split_whitespace() {
                sentence_start = prose_word(raw, heading, sentence_start, &mut stats);
            }
        }
    }
    let mut found: Vec<(usize, String)> = stats
        .into_iter()
        .filter(|(_, s)| s.qualifies())
        .filter_map(|(_, s)| Some((s.order, s.best()?.to_string())))
        .collect();
    found.sort();
    found.into_iter().map(|(_, w)| w).collect()
}

fn stat<'a>(stats: &'a mut HashMap<String, Stat>, word: &str) -> &'a mut Stat {
    let order = stats.len();
    stats.entry(word.to_lowercase()).or_insert_with(|| Stat {
        order,
        ..Stat::default()
    })
}

/// One whitespace-separated prose token. Returns whether the next token
/// starts a sentence.
fn prose_word(
    raw: &str,
    heading: bool,
    sentence_start: bool,
    stats: &mut HashMap<String, Stat>,
) -> bool {
    let start = raw.find(char::is_alphanumeric).unwrap_or(raw.len());
    let end = raw
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(start)
        .max(start);
    let (lead, core, trail) = (&raw[..start], &raw[start..end], &raw[end..]);
    let ends =
        core.is_empty() && raw.contains('|') || trail.contains(['.', '?', '!', ':', ';', '|']);
    // Possessives count as the word ("Tidewell's").
    let core = core
        .strip_suffix("'s")
        .or_else(|| core.strip_suffix("’s"))
        .unwrap_or(core);
    if core.is_empty() || !core.chars().all(char::is_alphanumeric) || is_stopword(core) {
        // File names, paths, URLs, snake_case, hyphenated English: not prose terms.
        return ends || core.is_empty() && sentence_start;
    }
    let has_digit = core.chars().any(|c| c.is_ascii_digit());
    let has_alpha = core.chars().any(char::is_alphabetic);
    let has_lower = core.chars().any(char::is_lowercase);
    let first_upper = core.chars().next().is_some_and(char::is_uppercase);

    if has_digit && has_alpha {
        let s = stat(stats, core);
        s.strong = true;
        s.spelled(core);
    } else if has_digit {
        // A number.
    } else if has_inner_case(core) && has_lower {
        let s = stat(stats, core);
        s.strong = true;
        s.spelled(core);
    } else if !has_lower {
        if core.chars().count() >= 2 && core.chars().count() <= MAX_ACRONYM {
            let s = stat(stats, core);
            s.acronym = true;
            s.spelled(core);
        }
    } else if first_upper {
        if core.chars().count() >= 4 {
            let s = stat(stats, core);
            s.spelled(core);
            if !heading && !sentence_start && lead.is_empty() {
                s.capitalised_mid = true;
            }
        }
    } else {
        stat(stats, core).lowercase = true;
    }
    ends
}

/// `inline code`: a single camel or kebab identifier counts.
fn code_term(code: &str, stats: &mut HashMap<String, Stat>) {
    let code = code.trim();
    let ident = code.chars().next().is_some_and(char::is_alphabetic)
        && code.chars().all(|c| c.is_alphanumeric() || c == '-')
        && !code.ends_with('-');
    if !ident || is_stopword(code) {
        return;
    }
    let kebab = code.contains('-') && code.split('-').all(|p| !p.is_empty());
    let camel = has_inner_case(code) && code.chars().any(char::is_lowercase);
    if kebab || camel {
        let s = stat(stats, code);
        s.strong = true;
        s.spelled(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(terms: &[String], w: &str) -> bool {
        terms.iter().any(|t| t == w)
    }

    #[test]
    fn keeps_jargon_skips_prose() {
        let doc = "\
# Getting Started With Widget

Widget is built on GPUI and talks to Tidewell. The Tidewell engine runs on an M4.
Open Settings to change it. We use tokio, and ClaudeBar shows it on macOS.
It ships as `squawk-core` and `keep_audio` and `ModelStatus`.

```rust
let HiddenThing = gpt9;
```
";
        let t = extract(doc);
        for w in [
            "GPUI",
            "Tidewell",
            "M4",
            "ClaudeBar",
            "macOS",
            "squawk-core",
            "ModelStatus",
        ] {
            assert!(has(&t, w), "{w} in {t:?}");
        }
        for w in [
            "Getting",
            "Started",
            "Widget",
            "Settings",
            "Open",
            "The",
            "tokio",
            "keep_audio",
            "HiddenThing",
            "gpt9",
        ] {
            assert!(!has(&t, w), "{w} in {t:?}");
        }
    }

    #[test]
    fn lowercase_use_disqualifies_capitalised_and_acronyms() {
        let t = extract("We like Rust a lot. rust is also a fungus. Say NOTE twice, a note.");
        assert!(!has(&t, "Rust"));
        assert!(!has(&t, "NOTE"));
    }

    #[test]
    fn sentence_starts_and_list_items_are_not_evidence() {
        let t = extract("- Parakeet runs here.\nWhisper was slower. | Table | Cell |\n");
        assert!(t.is_empty(), "{t:?}");
    }

    #[test]
    fn most_common_spelling_wins() {
        let t = extract("GitHub and GitHub and Github.");
        assert_eq!(t, vec!["GitHub".to_string()]);
    }

    #[test]
    fn possessive_counts_as_the_word() {
        let t = extract("It uses Tidewell's engine.");
        assert_eq!(t, vec!["Tidewell".to_string()]);
    }

    #[test]
    fn shouting_is_not_an_acronym() {
        let t = extract("This is IMPORTANT and you MUST read the TUI docs.");
        assert_eq!(t, vec!["TUI".to_string()]);
    }
}
